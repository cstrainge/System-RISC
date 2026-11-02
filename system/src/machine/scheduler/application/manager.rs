//! Canonical join rights. Detached readers retain the original removal and
//! acknowledge only after payload destruction, outside the manager guard.
use mac_object_table::registry::CleanupFailure as _;

use super::*;
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, cleanup::Failure};
type Rights = mac_object_table::store::Store<Arc<Right>, InvocationGate>;
struct Manager
{
    rights:   Rights,
    retiring: Option<Box<Retiring>>
}
static MANAGER: spin::Once<Mutex<Manager>> = spin::Once::new();
static CHANGED: AtomicBool = AtomicBool::new(false);
pub(super) fn changed()
{
    CHANGED.store(true, Ordering::Release);
    wake::request();
}
fn with<R>(action: impl FnOnce(&mut Manager) -> R) -> Result<R, Error>
{
    let manager = MANAGER.get().ok_or(Error::Admission { status: mac_abi::BAD_ABI,
                                                          stage:  "join manager initialization" })?;
    Ok(super::super::super::interrupts::with_masked(|| action(&mut manager.lock())))
}
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"TJST" } else { *b"TJRT" }),
                    label:    if storage
                    {
                        "Native join-right backing"
                    }
                    else
                    {
                        "Native join rights"
                    },
                    provider: "System native Thread Manager",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One native join-right value array \
                                                             per Process" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(crate) fn initialize() -> Result<(), Error>
{
    if !crate::context::current_thread_context().is_bootstrap() || MANAGER.get().is_some()
    {
        return Err(Error::Invalid("join manager initialization is boot-only"));
    }
    let kinds =
        crate::toolbox::handles::registry(|registry| {
            Ok::<_, mac_object_table::registry::Error>((registry.get(specification(false).kind)?,
                                                        registry.get(specification(true).kind)?))
        }).map_err(|_| {
              Error::Admission { status: mac_abi::BAD_ABI,
                                 stage:  "join registrations" }
          })?
          .map_err(|_| {
              Error::Admission { status: mac_abi::BAD_ABI,
                                 stage:  "join registrations" }
          })?;
    let manager = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
        Ok::<_, Error>(Manager { rights:   Rights::owned_prepared(kinds.0, kinds.1)?,
                                 retiring: None })
    })?;
    MANAGER.call_once(|| Mutex::new(manager));
    Ok(())
}
pub(super) fn prepare(owner: &OwnerRef, completion: Arc<Completion>)
                      -> Result<(Arc<Right>, PublicationPermit<InvocationGate>), Error>
{
    let permit = loop
    {
        let plan = with(|manager| manager.rights.plan_publication(owner))??;
        let preparation = plan.prepare()?;
        match with(|manager| manager.rights.admit_prepared(preparation))?
        {
            Ok((permit, old)) =>
            {
                old.dispose();
                break permit;
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    };
    let anchor = Box::try_new(Retiring { removal: None,
                                         next:    None }).map_err(|_| {
                                                             Error::Metadata("join retirement \
                                                                              anchor")
                                                         })?;
    let signal =
        Arc::try_new(ReadSignal { deleted: AtomicBool::new(false),
                                  owner:   owner.clone() }).map_err(|_| {
                                                               Error::Metadata("join release \
                                                                                signal")
                                                           })?;
    let right = Arc::try_new(Right { completion,
                                     claimant: Mutex::new(Claim::Open),
                                     anchor: Mutex::new(Some(anchor)),
                                     signal }).map_err(|_| Error::Metadata("join right"))?;
    Ok((right, permit))
}
pub(super) fn publish(permit: PublicationPermit<InvocationGate>, right: Arc<Right>)
                      -> Result<u64, Error>
{
    let Some(manager) = MANAGER.get()
    else
    {
        return Err(Error::Admission { status: mac_abi::BAD_ABI,
                                      stage:  "join manager" });
    };
    match super::super::super::interrupts::with_masked(|| {
              manager.lock().rights.publish_prepared(permit, right)
          })
    {
        Ok((publication, old)) =>
        {
            old.dispose();
            Ok(publication.id())
        }
        Err(failure) =>
        {
            let cause = failure.cause;
            drop(failure);
            Err(Error::Publication(cause))
        }
    }
}
pub(super) fn read(owner: &OwnerRef, id: u64) -> Result<RightRead, Error>
{
    let (right, key) = with(|manager| {
                           Some((manager.rights.get(id)?.clone(), manager.rights.key(id)?))
                       })?.ok_or(Error::Missing)?;
    if right.completion.owner.owner() != owner.owner()
    {
        return Err(Error::Foreign);
    }
    Ok(RightRead::new(right, key))
}

/// Caller holds SCHEDULER; this bounded lookup neither allocates nor drops an
/// owning value. Final child commitment can validate the same published right.
pub(super) fn live(id: u64, right: &Arc<Right>) -> Result<bool, Error>
{
    with(|manager| manager.rights.get(id).is_some_and(|actual| Arc::ptr_eq(actual, right)))
}
pub(super) fn consume(owner: &OwnerRef, id: u64) -> Result<(), Error>
{
    let read = read(owner, id)?;
    consume_read(&read, id)
}
/// The caller retains the exact live read obtained at operation admission. A
/// competing real disposal/walk may win invalidation; its original pending
/// ticket is checked, rather than inferring cleanup from !owner.accepting().
pub(super) fn consume_read(read: &RightRead, id: u64) -> Result<(), Error>
{
    let manager = MANAGER.get().ok_or(Error::Admission { status: mac_abi::BAD_ABI,
                                                          stage:  "join manager" })?;
    let result = super::super::super::interrupts::with_masked(|| {
        let mut manager = manager.lock();
        let live = manager.rights.get(id).is_some_and(|actual| Arc::ptr_eq(actual, &read.right));
        if !live
        {
            return if read.completion.owner.pending(read.key)
            {
                Ok(())
            }
            else
            {
                Err(Error::Admission { status: mac_abi::BAD_ABI,
                                       stage:  "admitted join right lost both live value and \
                                                original pending cleanup" })
            };
        }
        let Some(mut anchor) = read.anchor.lock().take()
        else
        {
            return Err(Error::Admission { status: mac_abi::BAD_ABI,
                                          stage:  "live join right lost its prepaid unique \
                                                   retirement anchor" });
        };
        match manager.rights.remove_with_backing(id)
        {
            Some(removed) =>
            {
                anchor.removal = Some(removed);
                anchor.next = manager.retiring.take();
                manager.retiring = Some(anchor);
                read.signal.deleted.store(true, Ordering::Release);
                Ok(())
            }
            None =>
            {
                // The same manager turn serializes ordinary consumes. Only
                // the genuine owner walk may have won owner invalidation.
                *read.anchor.lock() = Some(anchor);
                if read.completion.owner.pending(read.key)
                {
                    Ok(())
                }
                else
                {
                    Err(Error::Admission { status: mac_abi::BAD_ABI,
                                           stage:  "join invalidation refused without its \
                                                    original pending ticket" })
                }
            }
        }
    });
    if result.is_ok()
    {
        changed();
    }
    result
}
pub(crate) fn pump_rights()
{
    if !CHANGED.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let Some(manager) = MANAGER.get()
    else
    {
        return;
    };
    let mut list = super::super::super::interrupts::with_masked(|| manager.lock().retiring.take());
    while let Some(mut node) = list
    {
        list = node.next.take();
        if node.removal.as_ref().is_some_and(|removal| Arc::strong_count(&**removal) == 1)
        {
            drop(node);
        }
        else
        {
            super::super::super::interrupts::with_masked(|| {
                let mut manager = manager.lock();
                node.next = manager.retiring.take();
                manager.retiring = Some(node);
            });
        }
    }
}
pub(crate) struct ReleaseRights(pub bool);
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for ReleaseRights
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        if ticket.key().kind() != specification(self.0).kind
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        let Some(manager) = MANAGER.get()
        else
        {
            return Err(Failure::context_unavailable(ticket, "native join-right manager"));
        };
        if self.0
        {
            let released = super::super::super::interrupts::with_masked(|| {
                let mut manager = manager.lock();
                let preparing = manager.rights.publication_preparations(ticket.key().owner());
                if preparing != 0
                {
                    return Err(Failure::native_join(ticket, 0, preparing));
                }
                manager.rights.release_storage_with_backing(ticket).map_err(Failure::from)
            })?;
            drop(released);
            return Ok(());
        }
        let right = super::super::super::interrupts::with_masked(|| {
            manager.lock().rights.releasing(&ticket).cloned()
        });
        let Some(right) = right
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let readers = Arc::strong_count(&right);
        if readers != 2
        {
            return Err(Failure::native_join(ticket, readers.saturating_sub(2), 0));
        }
        let removed = super::super::super::interrupts::with_masked(|| {
            manager.lock().rights.release_with_backing(ticket)
        });
        drop(right);
        match removed
        {
            Ok(removed) =>
            {
                removed.dispose();
                Ok(())
            }
            Err(cause) => Err(cause.into())
        }
    }
}
