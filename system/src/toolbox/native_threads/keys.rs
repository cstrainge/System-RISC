//! Key birth prepares all owning metadata before its no-allocation publication.
use super::*;
pub(super) fn create(owner: &OwnerRef, image: crate::images::Binding, root: &Arc<Root>,
                     destructor: usize)
                     -> Result<u64, Error>
{
    if !owner.accepting() || root.closing.load(Ordering::Acquire)
    {
        return Err(Error::Finished);
    }
    // CREATE is already an allocation path outside slots/context guards.
    // Settle actual pending deletions before reporting capacity/budget refusal;
    // immediate delete/create reuse must not depend on a later timer/input turn.
    pump();
    let code = if destructor == 0
    {
        None
    }
    else
    {
        let code = crate::kernel::application_execution(owner, image).map_err(Error::Code)?;
        code.validate(owner, destructor).map_err(Error::Code)?;
        Some(code)
    };
    root.kernel(|| {
            let permit = permit(owner, |manager| &mut manager.keys)?;
            let position = interrupts::with_masked(|| {
                let mut positions = root.positions.lock();
                let at = positions.iter()
                                  .position(|used| !*used)
                                  .ok_or(Error::Capacity { limit: positions.len() })?;
                positions[at] = true;
                Ok::<_, Error>(Position { root: root.clone(),
                                          at })
            })?;
            let anchor =
                Box::try_new(Deleted { removal: None,
                                       next:    None }).map_err(|_| {
                                                           Error::Metadata("key retirement anchor")
                                                       })?;
            let signal = Arc::try_new(KeySignal { deleted: AtomicBool::new(false),
                                                 owner: owner.clone() }).map_err(|_| {
                                                     Error::Metadata("key retirement signal")
                                                 })?;
            let key = Arc::try_new(Key { destructor,
                                         _code: code,
                                         position,
                                         anchor: Spinlock::new(Some(anchor)),
                                         signal }).map_err(|_| Error::Metadata("key"))?;
            let Some(manager) = MANAGER.get()
            else
            {
                return Err(Error::Unavailable);
            };
            match interrupts::with_masked(|| manager.lock().keys.publish_prepared(permit, key))
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
        })
}
pub(super) fn delete(owner: &OwnerRef, id: u64) -> Result<(), Error>
{
    let actual = key(owner, id)?;
    let manager = MANAGER.get().ok_or(Error::Unavailable)?;
    let mut anchor = interrupts::with_masked(|| actual.anchor.lock().take()).ok_or(Error::Busy)?;
    let removed = interrupts::with_masked(|| manager.lock().keys.remove_with_backing(id));
    match removed
    {
        Some(removed) =>
        {
            actual.signal.deleted.store(true, Ordering::Release);
            anchor.removal = Some(removed);
            interrupts::with_masked(|| {
                let mut manager = manager.lock();
                anchor.next = manager.deleted.take();
                manager.deleted = Some(anchor);
            });
        }
        None =>
        {
            interrupts::with_masked(|| {
                *actual.anchor.lock() = Some(anchor);
            });
            return Err(Error::Missing);
        }
    }
    drop(actual);
    request_reap();
    Ok(())
}
