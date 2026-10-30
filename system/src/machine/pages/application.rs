//! Genuine buddy/Application-account binding. This factory derives its CPU
//! address only from the actual retained DirectMap-backed native Backing.
#![forbid(unsafe_code)]
use core::fmt;

use mac_kernel_model::{account::{Committed, Reserved},
                       heap,
                       policy::MemoryClass};
use mac_object_table::domain::{DomainId, DomainKind};

use super::{Backing, NativeInvocationGate};
use crate::toolbox::handles::OwnerRef;

type Extent = heap::Extent<'static, 'static, NativeInvocationGate>;

#[derive(Debug)]
pub(crate) enum ApplicationExtentError
{
    Pages(super::Error),
    Foreign
    {
        expected: DomainId,
        actual:   DomainId
    },
    Domain(DomainKind),
    Class(MemoryClass),
    Reservation
    {
        planned:  u64,
        reserved: u64
    },
    Backing
    {
        planned: u64,
        actual:  u64
    },
    Core(heap::Error)
}
impl ApplicationExtentError
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Pages(cause) => cause.status(),
            Self::Foreign { .. } | Self::Domain(_) | Self::Class(_) => mac_abi::ACCESS_DENIED,
            Self::Reservation { .. } | Self::Backing { .. } | Self::Core(_) => mac_abi::BAD_ABI
        }
    }
}
impl fmt::Display for ApplicationExtentError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pages(cause) => cause.fmt(out),
            Self::Foreign { expected,
                            actual } =>
            {
                write!(out,
                       "application backing receipt belongs to {actual}, expected actual heap \
                        owner {expected}")
            }
            Self::Domain(kind) =>
            {
                write!(out, "application heap requires a genuine Process owner, received {kind:?}")
            }
            Self::Class(class) => write!(out, "application backing receipt has class {class:?}"),
            Self::Reservation { planned,
                                reserved } =>
            {
                write!(out,
                       "application extent reservation differs from exact buddy plan: \
                        {reserved}/{planned} bytes")
            }
            Self::Backing { planned,
                            actual } =>
            {
                write!(out,
                       "application extent buddy backing differs from admission: \
                        {actual}/{planned} bytes")
            }
            Self::Core(cause) => cause.fmt(out)
        }
    }
}

// Variant fields have deliberate destruction order: metadata, actual pages,
// then the original reservation/charge. No cleanup ticket exists before the
// caller anchors/publishes the resulting extent in its canonical heap root.
enum HeldPreparation
{
    Reserved
    {
        _nodes: heap::PreparedNodes, _reservation: Reserved<NativeInvocationGate>
    },
    Backed
    {
        _nodes:       heap::PreparedNodes,
        _backing:     Backing,
        _reservation: Reserved<NativeInvocationGate>
    },
    Charged
    {
        _nodes:   heap::PreparedNodes,
        _backing: Backing,
        _charge:  Committed<NativeInvocationGate>
    }
}
/// Actual owned failure, not a copied accounting snapshot. Dispose outside
/// every heap/index/account/owner guard after recording its bounded cause.
#[must_use]
pub(crate) struct ApplicationExtentFailure
{
    pub(crate) cause: ApplicationExtentError,
    held:             HeldPreparation
}
impl ApplicationExtentFailure
{
    pub(crate) fn dispose(self) -> ApplicationExtentError
    {
        drop(self.held);
        self.cause
    }
}

/// The caller obtained this reservation from the actual canonical Process
/// Account before page acquisition. Matching labels below CHECK that binding;
/// they never construct an account, retarget a receipt or grant a process.
/// Growth policy and Kernel metadata preparation belong to that same real heap
/// operation outside its mutation guards. This factory adds no System-heap
/// occupancy receipt to physical application payload pages.
pub(crate) fn prepare_application_extent(owner: &OwnerRef,
                                         reservation: Reserved<NativeInvocationGate>,
                                         requested_backing: usize, nodes: heap::PreparedNodes)
                                         -> Result<Extent, ApplicationExtentFailure>
{
    let checked = (|| {
        if owner.owner().kind() != DomainKind::Process
        {
            return Err(ApplicationExtentError::Domain(owner.owner().kind()));
        }
        if reservation.owner() != owner.owner()
        {
            return Err(ApplicationExtentError::Foreign { expected: owner.owner(),
                                                         actual:   reservation.owner() });
        }
        if reservation.class() != MemoryClass::Application
        {
            return Err(ApplicationExtentError::Class(reservation.class()));
        }
        let planned =
            Backing::planned_bytes(requested_backing).map_err(ApplicationExtentError::Pages)?;
        if reservation.bytes() != planned
        {
            return Err(ApplicationExtentError::Reservation { planned,
                                                             reserved: reservation.bytes() });
        }
        Ok(planned)
    })();
    let planned = match checked
    {
        Ok(planned) => planned,
        Err(cause) =>
        {
            return Err(ApplicationExtentFailure { cause,
                                                  held: HeldPreparation::Reserved { _nodes:
                                                                                        nodes,
                                                                                    _reservation:
                                                                                        reservation } });
        }
    };
    let backing = match Backing::reserve_uninitialized(requested_backing)
    {
        Ok(backing) => backing,
        Err(cause) =>
        {
            return Err(ApplicationExtentFailure { cause: ApplicationExtentError::Pages(cause),
                                                  held:  HeldPreparation::Reserved { _nodes:
                                                                                         nodes,
                                                                                     _reservation:
                                                                                         reservation } });
        }
    };
    if backing.bytes() != planned
    {
        return Err(ApplicationExtentFailure {
            cause:ApplicationExtentError::Backing{planned,actual:backing.bytes()},
            held:HeldPreparation::Backed{_nodes:nodes,_backing:backing,_reservation:reservation}});
    }
    // Decomposition is private to the native owner; no raw Block extractor or
    // caller-selected virtual base is exposed. This exact Backing came from the
    // pinned real DirectMap and still uniquely owns its full buddy extent.
    let Backing { pages,
                  cpu_base } = backing;
    let charge = reservation.commit();
    match heap::Extent::prepare(pages,charge,cpu_base,nodes)
    {
        Ok(extent)=>Ok(extent),
        Err(failure)=>Err(ApplicationExtentFailure {
            cause:ApplicationExtentError::Core(failure.cause),
            held:HeldPreparation::Charged{_nodes:failure.nodes,
                _backing:Backing{pages:failure.backing,cpu_base},_charge:failure.charge}})
    }
}

/// Bounded buddy-base probes in physical coordinates. The DirectMap offset is
/// only page-aligned; aligning a CPU alias at larger orders is not valid.
/// The callback must authenticate the actual retained extent and its length.
pub(crate) fn application_wait_extent<T>(address: usize,
                                         mut visit: impl FnMut(usize, u64) -> Option<T>)
                                         -> Result<Option<T>, super::Error>
{
    use mac_kernel_model::pages::{PhysicalAddress, VirtualAddress};
    let pools = super::POOLS.get().ok_or(super::Error::Unavailable)?;
    let width = core::mem::size_of::<core::sync::atomic::AtomicU32>() as u64;
    let physical = match pools.ram.physical_address(VirtualAddress(address as u64), width)
    {
        Ok(address) => address.0,
        // Another genuine region resolver may own a non-direct-map address.
        Err(super::MapError::OutsideWindow { .. } | super::MapError::UnmappedRam { .. }) =>
        {
            return Ok(None);
        }
        Err(cause) => return Err(super::Error::Mapping(cause))
    };
    let mut bytes = crate::machine::memory::PAGE_BYTES;
    loop
    {
        let base = physical & !(bytes - 1);
        if let Ok(cpu) = pools.ram.virtual_address(PhysicalAddress(base), 1)
           && let Ok(cpu) = usize::try_from(cpu.0)
           && let Some(found) = visit(cpu, bytes)
        {
            return Ok(Some(found));
        }
        let Some(next) = bytes.checked_mul(2)
        else
        {
            break;
        };
        bytes = next;
    }
    Ok(None)
}
