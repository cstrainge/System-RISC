//! SBOX retained provenance is not CTRL identity or current mutable appearance.
//! Readers own immutable snapshots; canonical mutation and ACK need not wait.
#![forbid(unsafe_code)]
use mac_object_table::{domain::DomainId, store::CompletionKey};

use super::*;
use crate::toolbox::resources::definitions::RetainedDefinition;

/// Minted only from the actual original SBOX DRAW publication. Metadata alone
/// grants neither author admission nor Window/allocator authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollBoxPaintOrigin
{
    key:       CompletionKey,
    window:    u64,
    recipient: DomainId,
    source:    DefinitionRef
}
impl ScrollBoxPaintOrigin
{
    pub(super) fn publication(key: CompletionKey, object: &Object) -> Self
    {
        Self { key,
               window: object.window,
               recipient: object.recipient_owner,
               source: object.definition.reference() }
    }

    pub(crate) fn scroll_box_id(&self) -> u64
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

    pub(crate) fn source_matches(&self, source: &RetainedDefinition) -> bool
    {
        self.source.same(source.reference())
    }

    fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        let object = manager.objects.get(self.key.id()).ok_or(NOT_FOUND)?;
        if manager.objects.completion_key(self.key.id()) != Some(self.key)
           || object.window != self.window
           || object.recipient_owner != self.recipient
           || object.recipient != self.recipient.incarnation()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}
/// Actual committed raster/Session custody, separate from mutable object state.
/// The PaintPlan drops its original source before this raster and Activity.
pub(crate) struct ScrollBoxPaintSnapshot
{
    origin:   ScrollBoxPaintOrigin,
    _raster:  Arc<Raster>,
    #[cfg(guest)]
    recovery: Option<crate::recovery::ScrollBoxOrigin>
}
impl ScrollBoxPaintSnapshot
{
    pub(crate) fn origin(&self) -> ScrollBoxPaintOrigin
    {
        self.origin
    }

    pub(crate) fn with_scope<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        #[cfg(guest)]
        if let Some(origin) = &self.recovery
        {
            return origin.activity().run(operation);
        }
        Ok(operation())
    }
}
/// A readset, not an exclusion or SBOX ACK pin. Ordinary changes/removal may
/// proceed and invalidate this readset. Hold it in the canonical author owner
/// through actual RETURN/stopped, then destroy outside Desktop/manager guards.
pub(crate) struct ScrollBoxPaintClaim
{
    origin:    ScrollBoxPaintOrigin,
    spec:      Spec,
    top:       u32,
    highlight: i16,
    raster:    Arc<Raster>,
    source:    RetainedDefinition,
    #[cfg(guest)]
    recovery:  Option<crate::recovery::ScrollBoxOrigin>
}
impl ScrollBoxPaintClaim
{
    pub(crate) fn origin(&self) -> ScrollBoxPaintOrigin
    {
        self.origin
    }

    pub(crate) fn with_scope<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        #[cfg(guest)]
        if let Some(origin) = &self.recovery
        {
            return origin.activity().run(operation);
        }
        Ok(operation())
    }
}
impl Drawing
{
    pub(crate) fn paint_origin(&self) -> ScrollBoxPaintOrigin
    {
        self.origin
    }

    pub(crate) fn paint_snapshot(&self) -> ScrollBoxPaintSnapshot
    {
        ScrollBoxPaintSnapshot { origin:                                   self.origin,
                                 _raster:                                  self.raster.clone(),
                                 #[cfg(guest)]
                                 recovery:                                 self._recovery_origin
                                                                               .clone() }
    }
}
impl Manager
{
    fn paint_scope(&self, origin: &ScrollBoxPaintOrigin) -> Result<(), Status>
    {
        let object = self.objects.get(origin.key.id()).ok_or(NOT_FOUND)?;
        #[cfg(guest)]
        {
            if object.recovery != object._recovery_origin.is_some()
            {
                return Err(BAD_ABI);
            }
            match (&object._recovery_origin, crate::context::recovery_activity())
            {
                (Some(original), Some(current)) =>
                {
                    current.validate_scroll_box_origin(original,
                                                       origin.recipient,
                                                       origin.key.owner(),
                                                       mac_abi::WindowId(origin.window),
                                                       crate::recovery::ScrollBoxAccess::Live)?
                }
                (None, None) => (),
                _ => return Err(ACCESS_DENIED)
            }
        }
        #[cfg(not(guest))]
        let _ = object;
        Ok(())
    }

    /// Call in the actual original scope before capture/enqueue. The original
    /// Drawing/source may legitimately precede current Spec/top/raster state.
    pub(crate) fn claim_scroll_box_paint(&self, origin: &ScrollBoxPaintOrigin,
                                         drawn_source: &RetainedDefinition)
                                         -> Result<ScrollBoxPaintClaim, Status>
    {
        origin.validate(self)?;
        if !origin.source_matches(drawn_source)
        {
            return Err(ACCESS_DENIED);
        }
        self.paint_scope(origin)?;
        let object = self.objects.get(origin.key.id()).ok_or(NOT_FOUND)?;
        Ok(ScrollBoxPaintClaim { origin:                                   *origin,
                                 spec:                                     object.view.spec,
                                 top:                                      object.view.top,
                                 highlight:                                object.view.highlight,
                                 raster:                                   object.view
                                                                                 .raster
                                                                                 .clone(),
                                 source:                                   object.definition
                                                                                 .retain_source(),
                                 #[cfg(guest)]
                                 recovery:                                 object._recovery_origin
                                                                                 .clone() })
    }

    /// Revalidate before capture and replay alongside the real Window target
    /// claim/content readset and exact packet baseline. No numeric absence ACK.
    pub(crate) fn validate_scroll_box_paint(&self, held: &ScrollBoxPaintClaim)
                                            -> Result<(), Status>
    {
        held.origin.validate(self)?;
        self.paint_scope(&held.origin)?;
        let object = self.objects.get(held.origin.key.id()).ok_or(NOT_FOUND)?;
        if object.view.spec != held.spec
           || object.view.top != held.top
           || object.view.highlight != held.highlight
           || !Arc::ptr_eq(&object.view.raster, &held.raster)
           || !held.source.matches(&object.definition)
        {
            return Err(SERVICE_BUSY);
        }
        #[cfg(guest)]
        match (&object._recovery_origin, &held.recovery)
        {
            (Some(current), Some(original))
                if current.activity().same_session(original.activity()) =>
            {
                ()
            }
            (None, None) => (),
            _ => return Err(ACCESS_DENIED)
        }
        Ok(())
    }
}
