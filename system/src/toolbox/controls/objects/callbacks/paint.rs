//! Managed retained paint provenance and current-object exclusion are separate.
//! A committed appearance may intentionally lag the live object during Update.
use mac_object_table::{domain::DomainId, store::CompletionKey};

use super::*;

/// Minted only at the real Manager's retained publication boundaries. Copying
/// this provenance does not authorize a callback, allocate scratch or ACK work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedPaintOrigin
{
    key:       CompletionKey,
    window:    u64,
    recipient: u64
}
impl ManagedPaintOrigin
{
    pub(crate) fn control_id(&self) -> u64
    {
        self.key.id()
    }

    pub(crate) fn window(&self) -> u64
    {
        self.window
    }

    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.key.owner()
    }

    fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        let object = manager.objects.get(self.key.id()).ok_or(NOT_FOUND)?;
        if manager.objects.completion_key(self.key.id()) != Some(self.key)
           || object.window != self.window
           || object.recipient != self.recipient
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}

/// The current canonical object/source/tree readset, independently retained
/// from the exact original Drawing and its immutable definition-source token.
/// The eventual canonical author owner must drop this OUTSIDE Desktop, after
/// its actual Pending/Reply/scratch/readers have finished RETURN or stopped.
pub(crate) struct ManagedPaintClaim
{
    origin:         ManagedPaintOrigin,
    expected:       Expected,
    inherited_clip: Option<Rect>,
    #[cfg(systemrisc)]
    _activity:      Option<crate::recovery::Activity>,
    _allocation:    AllocationAuthority,
    // Last: actual reader/authority destruction precedes availability notices.
    claim:          Arc<Claim>
}
impl ManagedPaintClaim
{
    pub(crate) fn origin(&self) -> &ManagedPaintOrigin
    {
        &self.origin
    }
}

impl Manager
{
    fn paint_origin(&self, id: u64, window: u64) -> Result<ManagedPaintOrigin, Status>
    {
        if id == 0
        {
            return Err(BAD_ARGUMENT);
        }
        // A real managed control can be windowless, including the System boot
        // progress bar before Desktop exists. Its full CTRL completion key is
        // still provenance. This does not mint a Window canvas claim: callers
        // must independently admit their actual drawing target.
        let key = self.objects.completion_key(id).ok_or(NOT_FOUND)?;
        let object = self.objects.get(id).ok_or(NOT_FOUND)?;
        if object.window != window
        {
            return Err(ACCESS_DENIED);
        }
        Ok(ManagedPaintOrigin { key,
                                window,
                                recipient: object.recipient })
    }

    pub(in crate::toolbox::controls::objects) fn seal_managed_paint_origin(&self, window: u64,
                                                                           drawing: &mut super::super::super::Drawing)
                                                                           -> Result<(), Status>
    {
        let original = self.paint_origin(drawing.id, window)?;
        match drawing.origin
        {
            Some(held) if held != original => Err(SERVICE_BUSY),
            Some(_) => Ok(()),
            None =>
            {
                drawing.origin = Some(original);
                Ok(())
            }
        }
    }

    /// All grid/table/window-root publication uses this SAME actual Prepared
    /// carrier. Validate the entire set before adding any provenance; refusal
    /// precedes retained removal/append or current-object mutation.
    pub(crate) fn seal_prepared_paint_origins(&self, window: u64, prepared: &mut Prepared)
                                              -> Result<(), Status>
    {
        for drawing in &prepared.drawings
        {
            let original = self.paint_origin(drawing.id, window)?;
            if drawing.origin.is_some_and(|held| held != original)
            {
                return Err(SERVICE_BUSY);
            }
        }
        for drawing in &mut prepared.drawings
        {
            // SAME exclusive Desktop turn and no effects between validation
            // and these scalar publications; existing seals remain unchanged.
            self.seal_managed_paint_origin(window, drawing)?;
        }
        Ok(())
    }

    fn paint_recovery_scope(&self, origin: &ManagedPaintOrigin) -> Result<(), Status>
    {
        let object = self.objects.get(origin.key.id()).ok_or(NOT_FOUND)?;
        #[cfg(systemrisc)]
        {
            match (&object._recovery_activity, crate::context::recovery_activity())
            {
                (None, None) => (),
                (Some(original), Some(current))
                    if current.recipient().owner() == original.recipient().owner()
                       && current.recipient().owner().incarnation() == origin.recipient
                       && current.storage_owner().owner() == origin.key.owner()
                       && current.storage_owner().owner() == original.storage_owner().owner()
                       && current.root_subject()
                                 .scope()
                                 .same_scope(original.root_subject().scope())
                       && crate::context::io_subject().is_some_and(|subject| {
                                                          subject.scope()
                                                                 .same_scope(current.root_subject()
                                                                                    .scope())
                                                      }) =>
                {
                    ()
                }
                _ => return Err(ACCESS_DENIED)
            }
        }
        #[cfg(not(systemrisc))]
        let _ = object;
        Ok(())
    }

    /// A real origin is required; a raw Drawing id / UnclaimedSubject cannot
    /// elect a managed object. Admission is fallible before any author effect.
    pub(crate) fn claim_managed_paint(&mut self, origin: &ManagedPaintOrigin,
                                      allocation: &AllocationAuthority)
                                      -> Result<ManagedPaintClaim, Status>
    {
        origin.validate(self)?;
        if allocation.owner() != origin.key.owner()
        {
            return Err(ACCESS_DENIED);
        }
        self.paint_recovery_scope(origin)?;
        // Existing genuine ancestor/descendant overlap discipline. A parent
        // snapshot and any child operation cannot replace one another's claim.
        self.no_claims_on_tree(origin.key.id())?;
        let inherited_clip = self.inherited_clip(origin.key.id());
        let object = self.objects.get_mut(origin.key.id()).ok_or(NOT_FOUND)?;
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        let (claim, return_record) = crate::allocator::with_target(target, || {
            let record =
                super::super::claim_wait::ClaimReturn::prepare(self.claim_presence.clone())?;
            let claim = Arc::try_new(Claim::new(record.clone())).map_err(|_| NO_MEMORY)?;
            Ok::<_, Status>((claim, record))
        })?;
        return_record.bind()?;
        // Last fallible work was above. Capture CURRENT binding/tree separately
        // from committed Drawing/source. No retained appearance comparison.
        let expected = Expected { key:       origin.key,
                                  recipient: object.recipient,
                                  tree:      object.tree,
                                  source:    object.definition.quiescence_plan() };
        #[cfg(systemrisc)]
        let activity = object._recovery_activity.clone();
        object.claim = Some(ClaimLink { claim:         Arc::downgrade(&claim),
                                        whole_window:  false,
                                        return_record: return_record.clone() });
        Ok(ManagedPaintClaim { origin: *origin,
                               expected,
                               inherited_clip,
                               #[cfg(systemrisc)]
                               _activity: activity,
                               _allocation: allocation.clone(),
                               claim })
    }

    pub(crate) fn validate_managed_paint(&self, held: &ManagedPaintClaim) -> Result<(), Status>
    {
        held.origin.validate(self)?;
        self.paint_recovery_scope(&held.origin)?;
        held.expected.validate(self, &held.claim, held.origin.window)?;
        if self.inherited_clip(held.origin.key.id()) != held.inherited_clip
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}
