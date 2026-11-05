//! Controller-scoped leases of a pooled block's independently usable resources.
//!
//! Some blocks expose several resources that are used one at a time by different
//! owners: the DMA controller has fifteen channels, a GPIO controller has dozens
//! of pins. A device claim in this system is exclusive per *page*, which is right
//! for the block and wrong for those resources: a driver that needs one channel
//! would have to claim the whole controller, and then no second driver could
//! exist.
//!
//! This is the arbiter for that shape. One owner holds the page; this ledger
//! hands out the individual resources inside it. It deliberately does not change
//! the device claim: the page has one owner, exactly as before, and the resources
//! are leased from that owner's declaration rather than being claimed from the
//! kernel.
//!
//! The same ledger serves a GPIO controller, which is why nothing here knows what
//! a channel is: a resource is a bit position in the pool the record declares.
//!
//! Lifetime rules, which are the pipe facility's rather than new ones here:
//!
//! * a lease belongs to its owner and to no one else;
//! * releasing it makes the resource available again immediately;
//! * an owner that goes away has every lease released, so a crashed module cannot
//!   strand a channel;
//! * a refusal names the resource and who holds it, because "busy" with no
//!   further information is the error-invariant defect.

/// One lease: which pool, which resource in it, and who holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lease
{
    pub key:      [u8; 16],
    pub resource: u32,
    pub owner:    OwnerId
}

/// Who holds a lease. Module instances are identified by their admission, so an
/// owner's death is observable as the admission ending.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OwnerId(pub u32);

/// What a caller needs. Most callers need *a* resource and do not care which;
/// one that cares is one with a fixed wiring or a fixed register field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Requirement
{
    Any,
    Exactly(u32)
}

/// Every way a lease request can fail, each naming the fact that refused it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal
{
    /// No block with this identity was declared as pooled here.
    UnknownPool
    {
        key: [u8; 16]
    },
    /// The pool exists but every resource in it is leased.
    Exhausted
    {
        key: [u8; 16]
    },
    /// The named resource exists but someone else holds it.
    Held
    {
        resource: u32,
        owner:    OwnerId
    },
    /// The named resource is outside the pool the board declares.
    NotInPool
    {
        resource: u32
    },
    /// A release named a lease this owner does not hold.
    NotHeld,
    /// The ledger has no room for another pool or lease.
    Full
}

/// Pools and leases are bounded because this is resident state, allocated once
/// and never grown. Four pools covers the controllers this platform drives; the
/// lease bound is what a boot-time driver set can hold at once.
pub const MAX_POOLS: usize = 4;
pub const MAX_LEASES: usize = 24;

#[derive(Clone, Copy)]
struct Pool
{
    key:   [u8; 16],
    mask:  u64
}

/// Pools declared from the platform record and the leases issued against them.
pub struct Ledger
{
    pools:  [Option<Pool>; MAX_POOLS],
    leases: [Option<Lease>; MAX_LEASES]
}

impl Ledger
{
    pub const fn new() -> Self
    {
        Self { pools:  [None; MAX_POOLS],
               leases: [None; MAX_LEASES] }
    }

    /// Declare a pooled block from the record's mask. Declaring the same block
    /// twice with the same mask is not an error; with a different mask it is,
    /// because two answers to one question is how a pool and its record drift.
    pub fn declare(&mut self, key: [u8; 16], mask: u64) -> Result<(), Refusal>
    {
        if mask == 0
        {
            return Err(Refusal::Exhausted { key });
        }
        if let Some(existing) = self.pools.iter().flatten().find(|pool| pool.key == key)
        {
            return if existing.mask == mask { Ok(()) } else { Err(Refusal::NotHeld) };
        }
        let slot = self.pools.iter_mut().find(|slot| slot.is_none()).ok_or(Refusal::Full)?;
        *slot = Some(Pool { key, mask });
        Ok(())
    }

    /// Lease one resource from `key` for `owner`.
    pub fn lease(&mut self, owner: OwnerId, key: [u8; 16], requirement: Requirement)
                 -> Result<Lease, Refusal>
    {
        let mask = self.pools
                       .iter()
                       .flatten()
                       .find(|pool| pool.key == key)
                       .map(|pool| pool.mask)
                       .ok_or(Refusal::UnknownPool { key })?;
        let candidate = match requirement
        {
            Requirement::Exactly(resource) =>
            {
                if resource >= 64 || mask & (1 << resource) == 0
                {
                    return Err(Refusal::NotInPool { resource });
                }
                if let Some(holder) = self.holder(key, resource)
                {
                    return Err(Refusal::Held { resource, owner: holder });
                }
                resource
            }
            Requirement::Any =>
            {
                match (0..64u32).find(|resource| {
                                            mask & (1u64 << resource) != 0
                                            && self.holder(key, *resource).is_none()
                                        })
                {
                    Some(resource) => resource,
                    None => return Err(Refusal::Exhausted { key })
                }
            }
        };
        let slot =
            self.leases.iter_mut().find(|slot| slot.is_none()).ok_or(Refusal::Full)?;
        let lease = Lease { key,
                            resource: candidate,
                            owner };
        *slot = Some(lease);
        Ok(lease)
    }

    /// Give a resource back. Only its holder may do this, and giving back
    /// something this owner does not hold is refused rather than ignored: a
    /// silent no-op here would leave a leaked resource that nothing can see.
    pub fn release(&mut self, owner: OwnerId, lease: Lease) -> Result<(), Refusal>
    {
        let slot = self.leases
                       .iter_mut()
                       .find(|slot| {
                           slot.is_some_and(|held| {
                               held.key == lease.key
                               && held.resource == lease.resource
                               && held.owner == owner
                           })
                       })
                       .ok_or(Refusal::NotHeld)?;
        *slot = None;
        Ok(())
    }

    /// Drop every lease an owner holds. Called when an owner's admission ends,
    /// so a module that dies mid-transfer does not keep a channel.
    pub fn release_owner(&mut self, owner: OwnerId) -> usize
    {
        let mut freed = 0;
        for slot in &mut self.leases
        {
            if slot.is_some_and(|held| held.owner == owner)
            {
                *slot = None;
                freed += 1;
            }
        }
        freed
    }

    /// The resources in `key` that are currently free.
    pub fn available(&self, key: [u8; 16]) -> u64
    {
        let Some(pool) = self.pools.iter().flatten().find(|pool| pool.key == key)
        else
        {
            return 0;
        };
        let mut free = pool.mask;
        for lease in self.leases.iter().flatten().filter(|lease| lease.key == key)
        {
            if lease.resource < 64
            {
                free &= !(1u64 << lease.resource);
            }
        }
        free
    }

    /// A lease whose resource is in the pool the record declares. Checked so a
    /// consumer cannot act on a resource the board does not offer.
    pub fn holds(&self, owner: OwnerId, key: [u8; 16], resource: u32) -> bool
    {
        self.leases
            .iter()
            .flatten()
            .any(|lease| lease.owner == owner && lease.key == key && lease.resource == resource)
    }

    fn holder(&self, key: [u8; 16], resource: u32) -> Option<OwnerId>
    {
        self.leases
            .iter()
            .flatten()
            .find(|lease| lease.key == key && lease.resource == resource)
            .map(|lease| lease.owner)
    }
}

/// The ledger's own invariants, exercised once at start-up.
///
/// This is a check of the arbiter, not of a device: it declares a pool, leases
/// from it, is refused when the same resource is asked for twice, leases a
/// different resource concurrently, and observes a release make the resource
/// available again. It prints what it observed so a boot log carries the
/// evidence rather than an assurance.
///
/// A platform that declares no pooled block is still checked, because the ledger
/// is the thing under test and it must behave the same everywhere. The pool it
/// uses there is stated in the log line, so it cannot be mistaken for a device.
pub fn self_check()
{
    // The record's own pointer pools, so a mistake in the mask shows up here
    // rather than in the first driver that tries to lease a channel.
    let declared = crate::machine::power::platform::PLATFORM_DEVICE_POOLS;
    let (key, mask, source) = match declared.first()
    {
        Some(&(key, mask)) => (key, mask, "record"),
        None => (*b"self-check-pool\0", 0x0000_0007u64, "self-check")
    };
    let mut ledger = Ledger::new();
    if ledger.declare(key, mask).is_err()
    {
        serial!("SYSTEM_LEASE_PROBE: refused declaration source={source}");
        return;
    }
    let first = OwnerId(1);
    let second = OwnerId(2);
    let Ok(lease) = ledger.lease(first, key, Requirement::Any) else
    {
        serial!("SYSTEM_LEASE_PROBE: no lease available source={source} pool={mask:#x}");
        return;
    };
    serial!("SYSTEM_LEASE_PROBE: granted source={source} resource={} pool={mask:#x}",
            lease.resource);
    match ledger.lease(second, key, Requirement::Exactly(lease.resource))
    {
        Err(Refusal::Held { resource, owner }) =>
        {
            serial!("SYSTEM_LEASE_PROBE: refused resource={resource} holder={}", owner.0)
        }
        other =>
        {
            serial!("SYSTEM_LEASE_PROBE: unexpected second lease {other:?}");
            return;
        }
    }
    let Ok(other) = ledger.lease(second, key, Requirement::Any) else
    {
        serial!("SYSTEM_LEASE_PROBE: no concurrent lease");
        return;
    };
    serial!("SYSTEM_LEASE_PROBE: concurrent resource={} holder={}", other.resource,
            other.owner.0);
    if ledger.release(first, lease).is_err()
       || ledger.available(key) & (1u64 << lease.resource) == 0
    {
        serial!("SYSTEM_LEASE_PROBE: release did not free resource={}", lease.resource);
        return;
    }
    serial!("SYSTEM_LEASE_PROBE: released resource={} available={:#x}", lease.resource,
            ledger.available(key));
    // The owner-death path, which is the one a crash depends on.
    if ledger.release_owner(second) != 1 || ledger.available(key) != mask
    {
        serial!("SYSTEM_LEASE_PROBE: owner release did not return the pool");
        return;
    }
    serial!("SYSTEM_LEASE_PROBE: ok");
}
