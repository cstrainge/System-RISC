//! System-owned event routing, retained application drawing and compositing.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

pub(crate) use control_runtime::{TrackingStage, report_tracking_status};
use mac_abi::{events::{CMD_KEY, Event, EventKind},
              geometry::{Point, Rect},
              *};
use mac_object_table::shared::InvocationGate as _;

use super::quickdraw::{SurfaceExt,
                       fonts::FontManager,
                       fonts_service,
                       retained::{Content, RenderCommand, SceneTarget},
                       service};
use crate::{platform,
            toolbox::{events::{EventManager,
                               input::{Decoder, Input}},
                      handles::InvocationGate,
                      quickdraw::{Canvas, WHITE, font::GENEVA_9},
                      resources::definitions::DefinitionRef,
                      windows::{Action, WindowId, WindowManager, WindowPart}},
            virtio_input};

#[path = "menus/feedback.rs"]
mod menu_feedback;

/// Bootstrap must preserve a definition-publication transport refusal rather
/// than recasting it as a malformed resource or unrelated service failure.
#[derive(Debug)]
pub(crate) enum AdmissionError
{
    Service(Status),
    Definition(crate::loader::LoadError)
}
impl From<Status> for AdmissionError
{
    fn from(cause: Status) -> Self
    {
        Self::Service(cause)
    }
}
impl From<crate::loader::LoadError> for AdmissionError
{
    fn from(cause: crate::loader::LoadError) -> Self
    {
        Self::Definition(cause)
    }
}

#[path = "crashes/runtime.rs"]
mod crash_runtime;
#[path = "standard_file/runtime.rs"]
mod standard_file_runtime;
#[path = "dialogs/runtime.rs"]
mod dialog_runtime;
#[path = "dialogs/close.rs"]
mod dialog_close;
pub(crate) use dialog_close::{DialogCloseObserver, UserDialogClose};
#[path = "events/runtime.rs"]
mod event_runtime;
#[path = "drag/runtime.rs"]
mod drag_runtime;
pub(crate) use drag_runtime::native_drag;
#[path = "textedit/runtime.rs"]
mod textedit_runtime;
#[path = "quickdraw/ordinary_underlay.rs"]
mod ordinary_underlay;
#[path = "quickdraw/ordinary_transaction.rs"]
mod ordinary_transaction;
pub(crate) use ordinary_transaction::pump_ordinary_paint;
#[path = "controls/window_layout.rs"]
mod window_layout;
use ordinary_underlay::OrdinaryUnderlay;
#[path = "processes/runtime.rs"]
mod local;
pub(crate) use local::{FinderAdmission, FinderCapacityError, FinderPreparation,
                       system_report::{SystemReportAction, SystemReportActionKind,
                                       active as system_report_active,
                                       changed as system_report_changed,
                                       pump as pump_system_report}};
#[path = "windows/runtime.rs"]
mod window_runtime;
#[path = "windows/input_callbacks.rs"]
mod input_callbacks;
#[path = "windows/tracking_grow.rs"]
mod tracking_grow;
#[path = "windows/close_ui.rs"]
mod close_ui;
pub(crate) use input_callbacks::pump_window_input;
#[path = "windows/native_operations.rs"]
mod native_operations;
pub(crate) use native_operations::{BeginError as WindowBeginError, TrustedWindowClose,
                                   UserWindowOperation};
pub(crate) use window_runtime::{BirthProgress as UserWindowProgress, UserBirth as UserWindowBirth,
                                native_window};
#[path = "controls/runtime.rs"]
mod control_runtime;
pub(crate) use control_runtime::{admit_offscreen, offscreen_toolbox};
#[path = "controls/scrollbox/runtime.rs"]
mod scroll_box_runtime;
pub(crate) use control_runtime::{AutomaticPrelude as AutomaticControlPrelude,
                                 AutomaticProgress as AutomaticControlProgress,
                                 UserBirth as UserControlBirth,
                                 UserOperation as UserControlOperation,
                                 UserOperationProgress as UserControlOperationProgress,
                                 native_control};
pub(crate) use scroll_box_runtime::UserOperation as UserScrollBoxOperation;
fn note_recovery_input_refusal(owner: u64, status: Status)
{
    use core::sync::atomic::{AtomicI32, Ordering};
    static LAST_REFUSAL: AtomicI32 = AtomicI32::new(OK);
    if LAST_REFUSAL.swap(status, Ordering::AcqRel) != status
    {
        serial!("RECOVERY_INPUT_CONTEXT_REFUSED app={} status={status}; modal authority retained",
                owner);
    }
}

const MAX_RETAINED_BYTES: usize = 2 * 1024 * 1024;
/// Scene retention, damage and compositing state for the screen. It owns no
/// window or application policy; Toolbox managers publish into it.
pub(in crate::toolbox) struct Presentation
{
    content:                               Vec<Content>,
    pending_content:                       Vec<Content>,
    content_cache:                         super::quickdraw::content_cache::ContentCache,
    ordinary_underlay:                     OrdinaryUnderlay,
    menu_cache:                            super::quickdraw::menu_cache::MenuBarCache,
    pending_video_mode:                    Option<mac_video_interface::Size>,
    video_mode_wait:                       bool,
    redraw:                                bool,
    content_damage:                        Vec<(SceneTarget, super::quickdraw::region::Region)>,
    presentation_damage:                   Vec<Rect>,
    presentation_copy_full:                bool,
    pub(in crate::toolbox) feedback_dirty: bool,
    feedback:                              super::quickdraw::feedback::Feedback,
    tracking_grow:                         Arc<tracking_grow::State>,
    outline_clip:                          super::quickdraw::feedback::VisibilityCache,
    highlight_clip:                        super::quickdraw::feedback::VisibilityCache,
    cursor_dirty:                          bool,
    frame_incomplete:                      bool,
    cursor_bounds:                         Rect,
    presentations:                         u64,
    compositions:                          u64
}

/// Pointer shape selection and busy-cursor tracking.
struct Pointer
{
    cursors:               super::quickdraw::cursors::Cursors,
    busy_cursor:           super::quickdraw::cursors::Cursors,
    report_cursor:         super::quickdraw::cursors::Cursors,
    report_cursor_visible: bool,
    responsiveness:        super::quickdraw::responsiveness::Responsiveness,
    text_cursor:           mac_abi::quickdraw::MonoCursor,
    resize_cursors:        [mac_abi::quickdraw::MonoCursor; 2],
    automatic_cursor:      Option<u32>
}

pub struct Toolbox
{
    pub(in crate::toolbox) drag:         super::drag::Manager,
    standard_file:                       super::standard_file::Manager,
    font_suitcases:                      super::font_suitcases::Manager,
    scroll_boxes:                        super::controls::scrollbox::Manager,
    controls:                            super::controls::objects::Manager,
    crash:                               super::crashes::Manager,
    system_report:                       Option<local::system_report::SystemReportSlot>,
    system_report_active:                Option<local::system_report::ReportSession>,
    system_ui_baseline:                  bool,
    system_report_temporary:             bool,
    system_report_present_pending:       bool,
    system_report_cursor_pending:        bool,
    dialogs:                             super::dialogs::Alerts,
    core_dialogs:                        super::dialogs::Alerts,
    core_dialogs_active:                 bool,
    menus:                               super::menus::Manager,
    system_bar:                          super::menus::SystemBar,
    worlds:                              super::quickdraw::worlds::Worlds,
    classic_font:                        mac_abi::quickdraw::FontSelection,
    pub windows:                         WindowManager,
    first_window:                        WindowId,
    events:                              Box<EventManager>,
    textedit:                            super::textedit::Manager,
    pub(in crate::toolbox) presentation: Presentation,
    pointer:                             Pointer,
    /// Reported main-screen bounds (`qd.screenBits.bounds`); changed only by `display_changed`.
    pub(in crate::toolbox) screen:       Rect,
    decoders:                            [Decoder; 2],
    pub revision:                        u32,
    pub updates:                         u32,
    last_key:                            u8,
    last_action:                         &'static str,
    activity:                            u64,
    last_observed:                       (u64, u32, u64),
    cdef:                                DefinitionRef,
    system_cdef:                         DefinitionRef,
    mdef:                                DefinitionRef,
    owner:                               u64,
    foreground:                          u64,
    locals:                              Vec<local::Saved>,
    pub(in crate::toolbox) processes:    super::processes::Manager,
    menu_feedback:                       Option<menu_feedback::Feedback>,
    app_menu:                            super::processes::menu::ApplicationMenu,
    window_input:                        Arc<input_callbacks::State>
}

impl super::controls::objects::ControlHost for Toolbox
{
    fn managed_controls(&mut self) -> &mut super::controls::objects::Manager
    {
        &mut self.controls
    }
}
impl super::controls::scrollbox::ScrollBoxHost for Toolbox
{
    fn managed_scroll_boxes(&mut self) -> &mut super::controls::scrollbox::Manager
    {
        &mut self.scroll_boxes
    }
}
impl super::standard_file::FileDialogHost for Toolbox
{
    fn file_dialogs(&mut self) -> &mut super::standard_file::Manager
    {
        &mut self.standard_file
    }
}
impl super::font_suitcases::Host for Toolbox
{
    fn font_suitcases(&mut self) -> &mut super::font_suitcases::Manager
    {
        &mut self.font_suitcases
    }
}

fn window_creation_status(error: super::windows::manager::CreationError) -> Status
{
    use super::windows::manager::CreationError;
    match error
    {
        CreationError::NoMemory => NO_MEMORY,
        CreationError::Binding(status) => status,
        CreationError::Facility(mac_object_table::domain::PoolError::ForeignOwner { .. }) =>
            ACCESS_DENIED,
        CreationError::Facility(error) =>
        {
            super::handles::registry_status(&mac_object_table::registry::Error::Pool(error))
        }
        CreationError::Stopping | CreationError::RecoveryBusy => SERVICE_BUSY,
        CreationError::CapacityChanged => SERVICE_BUSY,
        CreationError::Publication(cause) => match cause {
            mac_object_table::store::PreparedError::CapacityChanged => SERVICE_BUSY,
            mac_object_table::store::PreparedError::Pool(
                mac_object_table::domain::PoolError::ForeignOwner { .. }) => ACCESS_DENIED,
            mac_object_table::store::PreparedError::Pool(cause) =>
                super::handles::registry_status(&mac_object_table::registry::Error::Pool(cause)),
            mac_object_table::store::PreparedError::WrongStore
            | mac_object_table::store::PreparedError::RetiredStore => BAD_ABI
        },
        CreationError::RecoveryAuthority => ACCESS_DENIED,
        CreationError::Definition { reply,
                                    .. } => i32::try_from(reply).unwrap_or(BAD_ABI),
        CreationError::InvalidRequest | CreationError::Geometry { .. } => BAD_ARGUMENT
    }
}

impl Toolbox
{
    /// Separate the actual System storage owner from the app receiving a modal
    /// service. Only a genuine private Activity supplies that authority.
    pub(super) fn service_owners(
        &self)
        -> Result<(super::handles::OwnerRef, super::handles::OwnerRef), Status>
    {
        if let Some(activity) = crate::context::recovery_activity()
        {
            return Ok((activity.recipient(), activity.storage_owner()));
        }
        let owner = self.processes.ledger(self.owner)?;
        Ok((owner.clone(), owner))
    }

    pub(crate) fn boot_progress(&mut self, screen: &mut crate::boot_screen::BootScreen, stage: u8)
                                -> mac_formats::Result<()>
    {
        screen.completed(stage, &mut self.controls)
    }

    pub(crate) fn finish_boot_progress(&mut self, screen: &mut crate::boot_screen::BootScreen)
                                       -> mac_formats::Result<()>
    {
        screen.finish(&mut self.controls)
    }

    pub(crate) fn prepare_recovery_storage(&mut self) -> Result<(), AdmissionError>
    {
        crate::kernel::fonts().ok_or(NOT_SUPPORTED)?
            .require_direct_bitmap(fonts_service::selection(mac_abi::quickdraw::FontSelection::default()))
            .map_err(|status| {
                serial!("RECOVERY_SERVICE_ADMISSION_REFUSED service=System caption bitmap status={status}");
                status
            })?;
        let storage = super::handles::system_owner()?;
        let controls = super::standard_file::templates::required_control_count()?
            .max(super::dialogs::core_alerts::required_control_count()?)
            .max(Self::crash_control_count());
        // These are real occupied/reserved bootstrap allocations, not free
        // capacity that normal publishers may consume later. Global arrays
        // stay ordinary-backed; protected data is never an implicit upgrade.
        crate::context::without_recovery(|| {
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                // These services create the standard dialog window, standard
                // buttons and managed scrollbars. Load their genuine System
                // code now: later protected heap backing cannot admit missing
                // definition images or page metadata on their behalf.
                let source = crate::kernel::system_resources(&storage)?;
                crate::loader::prepare_system_definition(source, *b"WDEF", 0)?;
                for operation in
                    [mac_abi::controls::DRAW_STANDARD, mac_abi::controls::DRAW_SCROLLBAR]
                {
                    let id = super::controls::definition_id(&mac_abi::controls::ControlCall {
                        operation, ..Default::default()
                    })?;
                    crate::loader::prepare_system_definition(source, *b"CDEF", id)?;
                }
                self.windows.prepare_recovery(&storage).map_err(|status| { serial!("RECOVERY_SERVICE_ADMISSION_REFUSED service=Window Manager status={status}"); status })?;
                self.controls.prepare_recovery(&storage, controls).map_err(|status| { serial!("RECOVERY_SERVICE_ADMISSION_REFUSED service=Control Manager status={status}"); status })?;
                self.prepare_system_report_slot(&storage)?;
                self.prepare_core_dialogs().map_err(|status| {
                                                serial!("RECOVERY_SERVICE_ADMISSION_REFUSED \
                                                         service=Core Alert Manager \
                                                         status={status}");
                                                status
                                            })?;
                self.prepare_standard_file_storage(&storage).map_err(|status| { serial!("RECOVERY_SERVICE_ADMISSION_REFUSED service=Standard File Manager status={status}"); status })?;
                self.presentation.content.try_reserve_exact(2).map_err(|_| NO_MEMORY)?;
                self.presentation.pending_content.try_reserve_exact(2).map_err(|_| NO_MEMORY)?;
                self.presentation.content_damage.try_reserve_exact(3).map_err(|_| NO_MEMORY)?;
                self.presentation
                    .presentation_damage
                    .try_reserve_exact(4)
                    .map_err(|_| NO_MEMORY)?;
                self.presentation.ordinary_underlay.prepare(self.windows
                                                                 .recovery_window_capacity(),
                                                             self.presentation
                                                                 .content
                                                                 .capacity()
                                                                 .saturating_add(1))?;
                Ok(())
            })
        })
    }

    fn scene_spare(&self) -> usize
    {
        // Ordinary publishers preserve the two modal slots even while another
        // app owns the service. The actual service may consume its reservation.
        if crate::context::recovery_activity().is_some()
        {
            0
        }
        else
        {
            self.windows.recovery_window_capacity()
        }
    }

    fn retained_bytes(&self) -> usize
    {
        window_layout::ordinary_retained_bytes(&self.windows, &self.presentation.content,
                                               &self.presentation.pending_content)
    }

    fn content_mut(&mut self, id: impl Into<SceneTarget>) -> Result<&mut Content, Status>
    {
        let id = id.into();
        if !self.owns(id.window()?)
        {
            return Err(if self.windows.window(id.window()?).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        self.presentation
            .pending_content
            .iter_mut()
            .chain(&mut self.presentation.content)
            .find(|content| content.id == id)
            .ok_or(NOT_FOUND)
    }

    /// A retained update is private until EndUpdate. Other threads can poll,
    /// move the pointer and present the previously committed scene meanwhile.
    fn content_changed(&mut self, id: impl Into<SceneTarget>)
    {
        let id = id.into();
        if let Ok(window) = id.window()
           && let Some(window) = self.windows.window(window)
        {
            self.content_changed_rect(id, window.local_bounds());
        }
    }

    fn content_changed_rect(&mut self, id: impl Into<SceneTarget>, rect: Rect)
    {
        let id = id.into();
        if self.presentation.pending_content.iter().any(|content| content.id == id)
        {
            return;
        }
        self.content_changed_region(id, super::quickdraw::region::Region::from_rect(rect));
    }

    fn content_changed_region(&mut self, id: impl Into<SceneTarget>,
                              region: super::quickdraw::region::Region)
    {
        let id = id.into();
        use super::quickdraw::region::{BooleanOp, Region};
        if self.presentation.pending_content.iter().any(|content| content.id == id)
        {
            return;
        }
        let Some(window) = id.window().ok().and_then(|window| self.windows.window(window))
        else
        {
            return;
        };
        let local = window.local_bounds();
        let bounds = region.bounds();
        let region = if bounds.left >= local.left
                        && bounds.top >= local.top
                        && bounds.right <= local.right
                        && bounds.bottom <= local.bottom
        {
            region
        }
        else
        {
            region.combine(&Region::from_rect(local), BooleanOp::Intersection)
                  .unwrap_or_else(|_| {
                      let mut clipped = Rect::ZERO;
                      super::quickdraw::geometry::sect_rect(bounds, local, &mut clipped);
                      Region::from_rect(clipped)
                  })
        };
        if region.is_empty()
        {
            return;
        }
        for band in region.bands()
        {
            for span in &band.spans
            {
                self.presentation
                    .content_cache
                    .invalidate_rect(id, Rect::new(span.left, band.top, span.right, band.bottom));
            }
        }
        self.queue_content_damage(id, region);
    }

    fn queue_content_damage(&mut self, id: impl Into<SceneTarget>,
                            region: super::quickdraw::region::Region)
    {
        let id = id.into();
        if region.is_empty()
        {
            return;
        }
        if let Ok(window) = id.window()
        {
            // This changes only the canonical render dependency, not geometry
            // or object identity. Exhaustion disables author-paint reuse; the
            // accessor reports BAD_ABI instead of ever reusing an old stamp.
            let _ = self.windows.content_changed(window);
        }
        self.queue_scene_damage(id, region);
    }

    fn queue_scene_damage(&mut self, id: SceneTarget, region: super::quickdraw::region::Region)
    {
        if !self.system_scene(id)
        {
            self.presentation.ordinary_underlay.changed();
            match self.presentation.ordinary_underlay.queue_if_deferred(id, region)
            {
                Ok(()) => return,
                Err(pending) => return self.queue_active_content_damage(id, pending)
            }
        }
        self.queue_active_content_damage(id, region);
    }

    fn queue_active_content_damage(&mut self, id: SceneTarget,
                                   region: super::quickdraw::region::Region)
    {
        use super::quickdraw::region::{BooleanOp, Region};
        if region.is_empty()
        {
            return;
        }
        if let Some((_, previous)) =
            self.presentation.content_damage.iter_mut().find(|(window, _)| *window == id)
        {
            *previous=previous.combine(&region,BooleanOp::Union).unwrap_or_else(|_| {
                Region::from_rect(super::quickdraw::geometry::union_rect(previous.bounds(),region.bounds()))
            });
        }
        else
        {
            let spare = self.scene_spare();
            if self.presentation
                   .content_damage
                   .capacity()
                   .saturating_sub(self.presentation.content_damage.len())
               <= spare
               && shared_scene_allocation(|| {
                      self.presentation.content_damage.try_reserve(1 + spare)
                  }).is_err()
            {
                // Full composition is an explicit last resort when bounded
                // damage bookkeeping cannot be admitted; painting never panics.
                self.presentation.redraw = true;
                return;
            }
            self.presentation.content_damage.push((id, region));
        }
    }

    fn apply_content_outline(&mut self, id: impl Into<SceneTarget>, next: Option<Rect>)
                             -> Result<(), Status>
    {
        let id = id.into();
        let changed = self.windows.content_outline(id.window()?, next)?;
        self.presentation.feedback_dirty |= changed;
        Ok(())
    }

    pub(crate) fn apply_control_outline(&mut self, window: u64, next: Option<Rect>)
                                        -> Result<(), Status>
    {
        self.presentation.feedback_dirty |= self.windows.control_outline(WindowId(window), next)?;
        Ok(())
    }

    pub(crate) fn new(processes: super::processes::Manager,
                      finder_owner: super::handles::OwnerRef,
                      controls: super::controls::objects::Manager)
                      -> Result<Self, AdmissionError>
    {
        let finder = finder_owner.owner().incarnation();
        if processes.ledger(finder)?.owner() != finder_owner.owner()
           || processes.get(finder)?.image.is_none()
        {
            return Err(AdmissionError::Service(BAD_ABI));
        }
        let system_source = crate::kernel::system_resources(&super::handles::system_owner()?)?;
        // Resident defaults outlive Finder: never anchor them to its private
        // resource override merely because it is the bootstrap caller.
        crate::loader::prepare_system_definition(system_source, *b"WDEF", 0)?;
        let cdef = crate::loader::prepare_system_definition(system_source, *b"CDEF", 0)?;
        let system_cdef = cdef;
        let mdef = crate::loader::prepare_system_definition(system_source, *b"MDEF", 0)?;
        let text_cursor =
            crate::kernel::with_resources(|resources| {
                super::quickdraw::cursors::decode_mono(&resources.get(mac_abi::resources::SYSTEM,
                                                                      ResourceKey { kind: *b"CURS",
                                                                                    id:   1 })?
                                                                 .1
                                                                 .data)
            }).ok_or(NOT_SUPPORTED)??;
        let mut resize_cursors = [mac_abi::quickdraw::MonoCursor::default(); 2];
        for (slot, id) in resize_cursors.iter_mut().zip([17, 21])
        {
            *slot=crate::kernel::with_resources(|resources| {
                super::quickdraw::cursors::decode_mono(&resources.get(mac_abi::resources::SYSTEM,ResourceKey {kind:*b"CURS",id})?.1.data)
            }).ok_or(NOT_SUPPORTED)??;
        }
        let mut busy_cursor = super::quickdraw::cursors::Cursors::default();
        busy_cursor.resource_request(&mut mac_abi::quickdraw::CursorResourceCall {
            operation: mac_abi::quickdraw::CURSOR_RESOURCE_ANIMATE,
            scope: mac_abi::resources::SYSTEM,
            id: -6078,
            interval: 6,
            ..Default::default()
        }, Point::default(), Self::when(), |kind, id, scope| {
            crate::kernel::with_resources(|resources| {
                let (_,resource)=resources.get(scope,ResourceKey { kind,id })?;
                super::quickdraw::cursors::copy_resource(&resource.data)
            }).ok_or(NOT_SUPPORTED)?
        })?;
        let mut presentation_damage = Vec::new();
        shared_scene_allocation(|| presentation_damage.try_reserve_exact(4)).map_err(|_| {
                                                                                NO_MEMORY
                                                                            })?;
        let mut content_damage = Vec::new();
        shared_scene_allocation(|| content_damage.try_reserve_exact(1)).map_err(|_| NO_MEMORY)?;
        // The live Finder queue belongs to Finder. Its inactive row contains
        // the single System-owned empty placeholder used by enter's swaps.
        let allocation = processes.allocation(finder)?;
        let events = crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || Box::try_new(EventManager::new())).map_err(|_| NO_MEMORY)?;
        let mut locals = Vec::new();
        {
            shared_scene_allocation(|| locals.try_reserve_exact(1)).map_err(|_| NO_MEMORY)?;
        }
        locals.push(local::Saved::try_placeholder(processes.ledger(finder)?, mdef)?);
        let mut system_bar =
            crate::kernel::with_resources(|resources| {
                super::menus::SystemBar::new(mdef, crate::machine::traps::gateway, resources)
            }).ok_or(NOT_SUPPORTED)??;
        system_bar.reset(finder);
        let windows = WindowManager::with_kinds(super::handles::window_kinds()?,
                                                crate::machine::traps::gateway)?;
        let window_input =
            shared_scene_allocation(|| input_callbacks::State::new(windows.operation_notice()))?;
        let tracking_grow =
            shared_scene_allocation(|| tracking_grow::State::new(windows.operation_notice()))?;
        Ok(Self { first_window: WindowId(0),
                  crash: Default::default(),
                  system_report: None,
                  system_report_active: None,
                  system_ui_baseline: false,
                  system_report_temporary: false,
                  system_report_present_pending: false,
                  system_report_cursor_pending: false,
                  standard_file: super::standard_file::Manager::new(super::handles::file_dialog_kinds()?)?,
                  font_suitcases: super::font_suitcases::Manager::new(super::handles::font_suitcase_kinds()?)?,
                  scroll_boxes: super::controls::scrollbox::Manager::new(super::handles::scroll_box_kinds()?)?,
                  controls,
                  dialogs: super::dialogs::Alerts::new(super::handles::alert_kinds()?, processes.ledger(finder)?)?,
                  core_dialogs: super::dialogs::Alerts::new(super::handles::alert_kinds()?, super::handles::system_owner()?)?,
                  core_dialogs_active: false,
                  menus: super::menus::Manager::new(super::handles::menu_kinds()?, mdef, crate::machine::traps::gateway)?,
                  system_bar,
                  screen: Rect::new(0, 0, crate::display::width() as i16, crate::display::height() as i16),
                  pointer: Pointer { cursors: Default::default(), busy_cursor,
                                     report_cursor: Default::default(), report_cursor_visible: false,
                                     text_cursor, resize_cursors,
                                     responsiveness: Default::default(), automatic_cursor: None },
                  worlds: super::quickdraw::worlds::Worlds::new(super::handles::world_kinds()?)?,
                  classic_font: Default::default(),
                  windows,
                  events,
                  drag: { let (sessions, receivers) = super::handles::drag_kinds()?;
                          super::drag::Manager::new(sessions, receivers)? },
                  textedit: super::textedit::Manager::new(super::handles::editor_kinds()?, processes.ledger(finder)?)?,
                  presentation: Presentation { content: Vec::new(), pending_content: Vec::new(), content_cache: Default::default(), ordinary_underlay: Default::default(), menu_cache: Default::default(), pending_video_mode: None, video_mode_wait: false, redraw: true, content_damage, presentation_damage, presentation_copy_full:false, feedback_dirty: false, feedback: Default::default(), tracking_grow, outline_clip: Default::default(), highlight_clip: Default::default(), cursor_dirty: true, frame_incomplete: false, cursor_bounds: Rect::ZERO, presentations: 0, compositions: 0, },
                  decoders: [Decoder::new(), Decoder::new()],
                  revision: 0,
                  updates: 0,
                  last_key: 0,
                  last_action: "READY",
                  menu_feedback: None,
                  activity: 0,
                  last_observed: (u64::MAX, u32::MAX, u64::MAX),
                  cdef,system_cdef,mdef,owner:finder,foreground:finder,locals,
                  processes,
                  window_input,
                  app_menu:crate::kernel::with_resources(|resources| super::processes::menu::ApplicationMenu::new(mdef,crate::machine::traps::gateway,resources)).ok_or(NOT_SUPPORTED)?? })
    }

    fn uptime() -> u64
    {
        // Sixtieths of a second against the machine's admitted counter rate.
        // Before the monitor admits that rate there is nothing to convert
        // against, and a guessed rate would report elapsed time that never
        // happened, so uptime starts at zero instead.
        crate::machine::monitor::ticks_to_event_ticks(platform::ticks()).unwrap_or(0)
    }

    fn when() -> u32
    {
        Self::uptime() as u32
    }

    pub fn menu(&mut self, call: &mut mac_abi::menus::MenuCall) -> Status
    {
        if call.reserved != 0
        {
            return BAD_ARGUMENT;
        }
        if call.operation == mac_abi::menus::GET_BAR_HEIGHT
        {
            call.value = mac_abi::menus::MENU_BAR_HEIGHT as u32;
            return OK;
        }
        if call.operation == mac_abi::menus::SHOW_ABOUT_SYSTEM
        {
            if call.handle != 0
            {
                return BAD_ARGUMENT;
            }
            return self.show_about_system().map_or_else(|status| status, |_| OK);
        }
        if call.handle == 0 && call.menu_id == mac_abi::menus::SYSTEM_APPLE_MENU_ID
        {
            return self.system_bar.query(call).map_or_else(|status| status, |_| OK);
        }
        if call.handle == 0 && call.menu_id == super::processes::menu::MENU_ID
        {
            return self.app_menu.query(call).map_or_else(|e| e, |_| OK);
        }
        let owner = match self.processes.ledger(self.owner)
        {
            Ok(owner) => owner,
            Err(status) => return status
        };
        let result = crate::kernel::with_resources_in(call.scope, |resources| {
                         self.menus.request(&owner, call, resources, &self.system_bar)
                     }).unwrap_or_else(Err);
        if matches!(result, Ok(true)) && self.owner == self.foreground
        {
            self.refresh_system_menu();
            self.presentation.menu_cache.invalidate();
            self.presentation.redraw = true;
        }
        result.map_or_else(|status| status, |_| OK)
    }

    fn menu_selection(&mut self, selection: mac_abi::menus::Selection, when: u32, point: Point,
                      modifiers: u16)
    {
        self.events.post_system(Event { payload: 0,
                                        kind: EventKind::MenuCommand,
                                        message: ((selection.menu_id as u16 as u32) << 16)
                                                 | u32::from(selection.item),
                                        when,
                                        where_: point,
                                        modifiers,
                                        window: 0 });
        self.last_action = "MENU";
    }

    /// Validate the creation intent before a cold WDEF can be admitted. This
    /// observes current recovery availability; creation repeats the same checks.
    pub(crate) fn preflight_window(&self, operation: u32, request: &WindowRequest)
                                   -> Result<Option<i16>, Status>
    {
        if operation != NEW_WINDOW
        {
            return Ok(None);
        }
        if request.title_length as usize > request.title.len()
           || request.procedure < 0
           || !matches!(request.reserved, 0 | WINDOW_STAY_IN_BACK)
        {
            return Err(BAD_ARGUMENT);
        }
        core::str::from_utf8(&request.title[..request.title_length as usize]).map_err(|_| {
                                                                                 BAD_ARGUMENT
                                                                             })?;
        let (recipient, storage) = self.service_owners()?;
        if !recipient.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        self.windows
            .preflight_creation(request.bounds,
                                &storage,
                                (request.minimum_width, request.minimum_height),
                                request.reserved == WINDOW_STAY_IN_BACK)
            .map_err(window_creation_status)?;
        Ok(Some(request.procedure >> 4))
    }

    pub fn window(&mut self, operation: u32, request: &mut WindowRequest) -> Status
    {
        if operation == NEW_WINDOW
        {
            return self.create_window(request, None);
        }
        if operation != GET_WINDOW
        {
            crate::trace::record(crate::trace::WINDOW,
                                 self.owner,
                                 operation as u64,
                                 request.id as u64,
                                 0,
                                 0);
        }
        let mut result = OK;
        let when = Self::when();
        if request.id == 0 && operation == GET_WINDOW
        {
            let Some(id) = self.windows.front_readable(self.owner)
            else
            {
                return NOT_FOUND;
            };
            request.id = id.0;
        }
        if operation != NEW_WINDOW
           && request.id != 0
           && !(if operation == GET_WINDOW
           {
               self.windows.readable_by(WindowId(request.id), self.owner)
           }
           else
           {
               self.owns(WindowId(request.id))
           })
        {
            return if self.windows.window(WindowId(request.id)).is_some()
            {
                ACCESS_DENIED
            }
            else
            {
                NOT_FOUND
            };
        }
        let id = WindowId(request.id);
        let mut patch_damage = None;
        match operation
        {
            CLOSE_WINDOW =>
            {
                #[cfg(systemrisc)]
                if match self.windows.plan_trusted_close(id)
                {
                    Ok(plan) => plan.is_some(),
                    Err(status) => return status
                }
                {
                    return match self.request_trusted_window_close(id)
                    {
                        Ok(handle) =>
                        {
                            match handle.poll_result()
                            {
                                Some(Ok(_)) => OK,
                                Some(Err(super::windows::manager::CloseError::NotFound(_))) =>
                                {
                                    NOT_FOUND
                                }
                                Some(Err(super::windows::manager::CloseError::Cleanup(cause))) =>
                                {
                                    close_ui::cleanup_status(cause)
                                }
                                None => SERVICE_BUSY
                            }
                        }
                        Err(status) => status
                    };
                }
                if let Err(status) = self.controls.prepare_remove_window(id.0)
                {
                    // This close already planned its canonical job and armed the
                    // window's definition disposal. The kernel completes it with
                    // no further call from this application: window_work::pump
                    // advances that same job, and control_work::pump drains the
                    // disposal, both from the scheduler maintenance path. A stock
                    // application reads Service Busy as a terminal failure and
                    // abandons the close it just started, so report the in-flight
                    // close as accepted. Creation against the window is refused
                    // while its disposal is pending, so no caller can observe a
                    // half-disposed window, and a stuck advance still reports
                    // CONTROL_DISPOSAL_RETAINED with its canonical job and tickets.
                    if status == SERVICE_BUSY && self.controls.disposal_pending_window(id.0)
                    {
                        return OK;
                    }
                    return status;
                }
                if let Err(status) = self.drag.prepare_window_close(id.0)
                {
                    return status;
                }
                // Closing a dialog's window also ends its modal protocol.
                // A parent already taken by explicit/generic close is absent.
                let alert = self.dialogs.take_window(id.0);
                match self.windows.dispose_window(id, &mut self.events, when)
                {
                    Ok(()) => (),
                    Err(super::windows::manager::CloseError::NotFound(_)) => return NOT_FOUND,
                    Err(super::windows::manager::CloseError::Cleanup(cause)) =>
                    {
                        serial!("WINDOW_CLOSE_REFUSED: owner={} {cause}", self.owner);
                        result = close_ui::cleanup_status(cause);
                    }
                }
                if let Err(status) = self.remove_closed_window_content(id, when)
                {
                    return status;
                }
                if let Some(alert) = alert
                {
                    let restored = self.restore_alert_focus(alert.previous);
                    if result == OK
                    {
                        result = restored.map_or_else(|status| status, |()| OK);
                    }
                }
            }
            SELECT_WINDOW =>
            {
                if self.owner != self.foreground
                   && let Err(status) = self.activate_process_window(self.owner, Some(id))
                {
                    return status;
                }
                if self.windows.window(id).is_none()
                {
                    return NOT_FOUND;
                }
                self.windows.select_window(id, &mut self.events, when);
                self.last_action = "SELECT";
            }
            GET_WINDOW =>
            {
                let id =
                    if id.0 == 0 { self.windows.front_window().unwrap_or_default() } else { id };
                let Some(window) = self.windows.window(id)
                else
                {
                    return NOT_FOUND;
                };
                request.id = id.0;
                request.bounds = window.bounds;
                let title = window.title();
                request.title.fill(0);
                request.title[..title.len()].copy_from_slice(title.as_bytes());
                request.title_length = title.len() as u32;
                request.minimum_width = window.min_width;
                request.minimum_height = window.min_height;
                request.procedure = window.procedure();
                request.reserved = i16::from(window.zoomed());
                return OK;
            }
            INVALIDATE_WINDOW => self.windows.invalidate(id, request.bounds),
            VALIDATE_WINDOW =>
            {
                return self.windows.validate(id).map_or_else(|error| error, |()| OK);
            }
            SET_WINDOW_BOUNDS =>
            {
                if let Err(status) = self.preflight_resident_window_geometry(id,
                    super::windows::manager::operations::Geometry::Bounds(request.bounds))
                { return status; }
                return match self.set_window_bounds(id, request.bounds)
                {
                    Ok(Some(action)) =>
                    {
                        self.action(action, when);
                        OK
                    }
                    Ok(None) => OK,
                    Err(status) => status
                };
            }
            SET_STANDARD_BOUNDS =>
            {
                let bounds = match request.reserved
                {
                    0 => Some(request.bounds),
                    1 => None,
                    _ => return BAD_ARGUMENT
                };
                if let Err(status) = self.preflight_resident_window_geometry(id,
                    super::windows::manager::operations::Geometry::Standard(bounds))
                { return status; }
                return self.windows.set_standard_bounds(id, bounds).map_or_else(|e| e, |()| OK);
            }
            SET_WINDOW_TITLE =>
            {
                let Some(bytes) = request.title.get(..request.title_length as usize)
                else
                {
                    return BAD_ARGUMENT;
                };
                let Ok(title) = core::str::from_utf8(bytes)
                else
                {
                    return BAD_ARGUMENT;
                };
                if !self.windows.set_title(id, title)
                {
                    return NOT_FOUND;
                }
            }
            SET_CLOSE_DEFERRED =>
            {
                if !matches!(request.reserved, 0 | 1)
                {
                    return BAD_ARGUMENT;
                }
                if !self.windows.set_close_deferred(id, request.reserved == 1)
                {
                    return NOT_FOUND;
                }
            }
            SET_GROW_ICON_CLIP =>
            {
                let clip = match request.reserved
                {
                    0 => Some(request.bounds),
                    1 => None,
                    _ => return BAD_ARGUMENT
                };
                if !self.windows.set_grow_icon_clip(id, clip)
                {
                    return NOT_FOUND;
                }
            }
            BEGIN_UPDATE =>
            {
                let Some(window) = self.windows.window(id).copied()
                else
                {
                    return NOT_FOUND;
                };
                if window.updating
                {
                    return BAD_ARGUMENT;
                }
                let spare = self.scene_spare();
                if shared_scene_allocation(|| {
                       self.presentation.pending_content.try_reserve(1 + spare)
                   }).is_err()
                {
                    return NO_MEMORY;
                }
                let Some(content) =
                    self.presentation.content.iter().find(|content| content.id == id)
                else
                {
                    return NOT_FOUND;
                };
                let (used, limit) = self.retained_admission(SceneTarget::from(id));
                if used.saturating_add(content.port.storage()) > limit
                {
                    return NO_MEMORY;
                }
                let mut pending = Content::new(id);
                pending.port = content.port.clone();
                if content.commands.is_core()
                {
                    let Some(activity) = crate::context::recovery_activity()
                    else
                    {
                        return ACCESS_DENIED;
                    };
                    let (frame, lease) = match activity.take_core_frame(id)
                    {
                        Ok(pair) => pair,
                        Err(status) => return status
                    };
                    pending.commands =
                        super::quickdraw::retained::Commands::from_core(frame, lease);
                }
                else if content.has_report_commands()
                {
                    let Some(activity) = crate::context::recovery_activity()
                    else
                    {
                        return ACCESS_DENIED;
                    };
                    if !content.report_binding(&activity, id)
                    {
                        return ACCESS_DENIED;
                    }
                    let (frame, lease) = match activity.take_report_frame(id)
                    {
                        Ok(pair) => pair,
                        Err(status) => return status
                    };
                    pending.commands =
                        super::quickdraw::retained::Commands::from_report(frame, lease);
                }
                // First cut repaints each complete rectangular content port.
                let local = window.local_bounds();
                self.windows.invalidate(id, local);
                let Some(damage) = self.windows.begin_update(id)
                else
                {
                    return NOT_FOUND;
                };
                request.bounds = damage;
                self.presentation.pending_content.push(pending);
                self.textedit.begin_cursor_update(id);
                self.updates += 1;
            }
            END_UPDATE =>
            {
                let Some(index) =
                    self.presentation.pending_content.iter().position(|content| content.id == id)
                else
                {
                    return BAD_ARGUMENT;
                };
                let (used, limit) = self.retained_admission(SceneTarget::from(id));
                let bounds = self.windows.window(id).unwrap().local_bounds();
                let Some(content) =
                    self.presentation.content.iter_mut().find(|content| content.id == id)
                else
                {
                    return NOT_FOUND;
                };
                let mut pending = self.presentation.pending_content.swap_remove(index);
                let outline = pending.outline.take();
                if let Some(mask) = pending.patch_mask().cloned()
                {
                    let prepared = (|| -> Result<super::quickdraw::region::Region, Status> {
                        let cursors = self.textedit.prepare_partial_cursor_update(id, &mask)?;
                        let mask = content.commit_patch(pending, bounds, used, limit)?;
                        cursors.commit();
                        Ok(mask)
                    })();
                    match prepared
                    {
                        Ok(mask) => patch_damage = Some(mask),
                        Err(status) =>
                        {
                            self.textedit.begin_cursor_update(id);
                            self.windows.end_update(id);
                            self.controls.layout_scene_changed(self.owner, id.0);
                            return status;
                        }
                    }
                }
                else
                {
                    *content = pending;
                    self.textedit.commit_cursor_update(id);
                }
                self.windows.end_update(id);
                self.controls.layout_scene_changed(self.owner, id.0);
                if let Some(outline) = outline
                   && let Err(status) = self.apply_content_outline(id, outline)
                {
                    return status;
                }
            }
            _ => return NOT_SUPPORTED
        }
        self.route_events();
        if operation == END_UPDATE
        {
            if let Some(mask) = patch_damage
            {
                self.content_changed_region(id.0, mask);
            }
            else
            {
                self.content_changed(id.0);
            }
        }
        else if !matches!(operation, BEGIN_UPDATE | INVALIDATE_WINDOW | SET_CLOSE_DEFERRED | SELECT_WINDOW)
        {
            self.presentation.redraw = true;
        }
        // Retained drawing is committed before EndUpdate returns. Applications
        // can report readiness only after the corresponding pixels are visible.
        if operation == END_UPDATE
        {
            self.present();
        }
        result
    }

    pub fn control_definition(id: i16) -> Result<DefinitionRef, Status>
    {
        crate::loader::cached_definition(*b"CDEF", id).and_then(|entry| {
                                                          entry.ok_or_else(|| {
                     mac_formats::Error::Missing("control definition was not prepared before \
                                                  object mutation").into()
                 })
                                                      })
                                                      .map_err(crate::loader::status)
    }

    pub(crate) fn preflight_scroll_box(&self, call: &mac_abi::scrollbox::Call)
                                       -> Result<bool, Status>
    {
        use mac_abi::scrollbox::*;
        let (recipient, storage) = self.service_owners()?;
        let owner = recipient.owner().incarnation();
        if self.scroll_boxes.bound_definition(call, owner)?.is_some()
        {
            return Ok(false);
        }
        let window = call.window;
        if window != 0 && !self.owns(WindowId(window))
        {
            return Err(if self.windows.window(WindowId(window)).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        if matches!(call.operation, CREATE | CONFIGURE)
           && window != 0
           && !self.windows.window(WindowId(window)).is_some_and(|w| {
                                                        let bounds = w.bounds;
                                                        let r = call.spec.viewport;
                                                        let b = call.spec.scrollbar;
                                                        r.left >= 0
                                                        && r.top >= 0
                                                        && r.right <= bounds.right - bounds.left
                                                        && r.bottom <= bounds.bottom - bounds.top
                                                        && b.left >= 0
                                                        && b.top >= 0
                                                        && b.right <= bounds.right - bounds.left
                                                        && b.bottom <= bounds.bottom - bounds.top
                                                    })
        {
            return Err(BAD_ARGUMENT);
        }
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        self.scroll_boxes.preflight_definition(&recipient, &storage, call, &fonts)?;
        Ok(crate::loader::cached_definition(*b"CDEF", 1).map_err(crate::loader::status)?.is_none())
    }

    pub fn scroll_box(&mut self, call: &mut mac_abi::scrollbox::Call)
                      -> Result<Option<super::controls::scrollbox::Drawing>, Status>
    {
        use mac_abi::scrollbox::*;
        let (recipient, storage) = self.service_owners()?;
        let owner = recipient.owner().incarnation();
        let window = if call.operation == CREATE
        {
            call.window
        }
        else
        {
            self.scroll_boxes.window(call.id, owner)?
        };
        if window != 0 && !self.owns(WindowId(window))
        {
            return Err(if self.windows.window(WindowId(window)).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        if matches!(call.operation, CREATE | CONFIGURE)
           && window != 0
           && !self.windows.window(WindowId(window)).is_some_and(|w| {
                                                        let bounds = w.bounds;
                                                        let r = call.spec.viewport;
                                                        let b = call.spec.scrollbar;
                                                        r.left >= 0
                                                        && r.top >= 0
                                                        && r.right <= bounds.right - bounds.left
                                                        && r.bottom <= bounds.bottom - bounds.top
                                                        && b.left >= 0
                                                        && b.top >= 0
                                                        && b.right <= bounds.right - bounds.left
                                                        && b.bottom <= bounds.bottom - bounds.top
                                                    })
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == DRAW && call.surface.is_null() && window == 0
        {
            return Err(BAD_ARGUMENT);
        }
        if matches!(call.operation, TRACK_BEGIN | TRACK_POLL)
           && window != 0
           && (self.owner != self.foreground
               || self.windows.active_window() != Some(WindowId(window)))
        {
            call.operation = TRACK_CANCEL;
        }
        if call.operation == TRACK_CANCEL
        {
            self.scroll_boxes.cancel(call, owner)?;
            self.clear_control_highlight(window, call.id, true);
            return Ok(None);
        }
        let entry = match self.scroll_boxes.bound_definition(call, owner)?
        {
            Some(entry) => entry,
            None => crate::toolbox::runtime::Toolbox::control_definition(1)?
        };
        let drawing = self.scroll_boxes.request(&recipient,
                                                 &storage,
                                                 call,
                                                 crate::kernel::fonts().ok_or(NOT_SUPPORTED)?
                                                                       .as_ref(),
                                                 entry,
                                                 crate::machine::traps::gateway)?;
        if !call.surface.is_null()
        {
            return Ok(drawing);
        }
        if call.operation == DISPOSE && window != 0
        {
            let mut removed = Vec::new();
            let mut coalesced = None;
            for content in self.presentation
                               .content
                               .iter_mut()
                               .chain(self.presentation.pending_content.iter_mut())
                               .filter(|content| content.id == SceneTarget::from(window))
            {
                content.retain_commands(|command| {
                    if let RenderCommand::ScrollBox(drawing) = command
                       && drawing.id == call.id
                    {
                        for rect in drawing.bounds()
                        {
                            accumulate_removed_damage(&mut removed, &mut coalesced, rect);
                        }
                    }
                    !matches!(command, RenderCommand::ScrollBox(drawing) if drawing.id == call.id)
                });
            }
            for rect in removed.into_iter().chain(coalesced)
            {
                self.content_changed_rect(window, rect);
            }
        }

        let mut drawing = drawing;
        if drawing.is_some()
        {
            let retired = self.publish_scroll_box_drawing(window, &mut drawing)?;
            // Existing synchronous trusted path; owned User continuations take
            // this same actual displaced command outside Desktop before Drop.
            drop(retired);
        }
        Ok(None)
    }

    fn publish_scroll_box_drawing(&mut self, window: u64,
                                  drawing: &mut Option<super::controls::scrollbox::Drawing>)
                                  -> Result<Option<RenderCommand>, Status>
    {
        let next = drawing.as_ref().ok_or(BAD_ARGUMENT)?;
        let bounds = next.bounds();
        let (used, limit) = self.retained_admission(SceneTarget::from(window));
        let content = self.content_mut(window)?;
        let retired = content.replace_scroll_box(drawing, used, limit)?;
        let old_bounds = match &retired
        {
            Some(RenderCommand::ScrollBox(old)) => Some(old.bounds()),
            _ => None
        };
        if !self.presentation
                .pending_content
                .iter()
                .any(|content| content.id == SceneTarget::from(window))
        {
            for rect in old_bounds.into_iter().flatten().chain(bounds)
            {
                self.content_changed_rect(window, rect);
            }
            self.present();
        }
        Ok(retired)
    }

    fn clear_control_highlight(&mut self, window: u64, id: u64, scroll_box: bool)
    {
        let mut removed = Vec::new();
        let mut coalesced = None;
        for content in self.presentation
                           .content
                           .iter_mut()
                           .chain(&mut self.presentation.pending_content)
                           .filter(|content| content.id == SceneTarget::from(window))
        {
            for command in content.iter_commands_mut()
            {
                let bounds = match command
                {
                    RenderCommand::Control(drawing) if !scroll_box && drawing.id == id =>
                    {
                        let damage = drawing.capture_bounds();
                        drawing.cancel_tracking().then_some(damage)
                    }
                    RenderCommand::ScrollBox(drawing) if scroll_box && drawing.id == id =>
                    {
                        drawing.cancel_tracking()
                    }
                    _ => None
                };
                if let Some(bounds) = bounds
                {
                    accumulate_removed_damage(&mut removed, &mut coalesced, bounds);
                }
            }
        }
        let changed = !removed.is_empty() || coalesced.is_some();
        for bounds in removed.into_iter().chain(coalesced)
        {
            self.content_changed_rect(window, bounds);
        }
        if changed
           && !self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window))
        {
            self.present();
        }
    }

    fn managed_control(&mut self, call: &mut mac_abi::controls::ControlCall,
                       diagnostic: &mut Option<TrackingStage>)
                       -> Result<(), Status>
    {
        use mac_abi::controls::*;
        if !call.surface.is_null()
           || call.reserved != 0
           || call.reflow > 1
           || (call.reflow != 0 && !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR))
        {
            return Err(BAD_ARGUMENT);
        }
        let (recipient, storage) = self.service_owners()?;
        let owner = recipient.owner().incarnation();
        let window = if matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            call.window
        }
        else
        {
            self.controls.window(call.id, owner)?
        };
        if window != 0 && !self.owns(WindowId(window))
        {
            return Err(if self.windows.window(WindowId(window)).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        // A deactivated/hidden owner cannot continue a capture after app switching.
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && window != 0
           && (self.owner != self.foreground
               || self.windows.active_window() != Some(WindowId(window)))
        {
            call.operation = CANCEL_TRACKING;
        }
        if call.operation == CANCEL_TRACKING
        {
            return self.cancel_control_tracking(call, owner, window);
        }
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && self.controls
                  .splitter_request(call, owner, crate::machine::traps::gateway)
                  .map_err(|status| {
                      TrackingStage::refusal(diagnostic, TrackingStage::SplitterRequest, status)
                  })?
        {
            let pending =
                self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window));
            let prepared = self.controls.prepare_splitter(call,
                                                           owner,
                                                           pending,
                                                           crate::machine::traps::gateway,
                                                           diagnostic)?;
            let (used, limit) = self.retained_admission(SceneTarget::from(window));
            return self.publish_control_layout(window, prepared, used, limit, diagnostic);
        }
        if call.operation == NEXT_CHILD_OBJECT
        {
            return self.controls.next_child(call, owner);
        }
        if call.operation == HIT_CHILDREN_OBJECT
        {
            let (child, part) =
                self.controls
                    .hit_children(call.id, owner, call.point, crate::machine::traps::gateway)?;
            call.layout.child = child;
            call.result = i32::from(part);
            return Ok(());
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message,
                       reflow::SET_CHILD
                       | reflow::SET_CHILD_INDEXED
                       | reflow::REMOVE_CHILD
                       | reflow::REPARENT)
        {
            // Membership publication cannot straddle an unfinished window paint
            // transaction. Refuse before consuming the child's owner; retry after
            // EndUpdate. No old scene is re-clipped against a half-built new one.
            let paint_pending =
                self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window));
            if self.controls.change_parent(call, owner, paint_pending)?
            {
                self.sync_control_tree(window);
            }
            return Ok(());
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message, reflow::GET_SCROLLING | reflow::GET_LOGICAL_RECT)
        {
            return self.controls.scroll_query(call, owner);
        }
        if (call.operation == REFLOW_OBJECT
            && matches!(call.layout.message,
                        reflow::SET_SCROLLING | reflow::SCROLL_TO | reflow::REVEAL_CHILD))
           || (matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
               && self.controls.scrolling(call.id, owner)?)
        {
            let pending =
                self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window));
            let prepared = self.controls.prepare_scroll(call,
                                                         owner,
                                                         pending,
                                                         crate::machine::traps::gateway,
                                                         |id| {
                                                             crate::toolbox::runtime::Toolbox::control_definition(id)
                                                         })?;
            let (used, limit) = self.retained_admission(SceneTarget::from(window));
            return self.publish_control_layout(window, prepared, used, limit, diagnostic);
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message,
                       reflow::SET_SPLITTER | reflow::GET_SPLITTER | reflow::RESET_SPLIT_SIZES)
        {
            return self.controls.splitter_configuration(call, owner);
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message, reflow::SET_GRID_TRACK | reflow::GET_GRID_TRACK)
        {
            return self.controls.grid_configuration(call, owner);
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message, reflow::SET_ANCHORS | reflow::SET_WINDOW_BORDER)
           && (call.layout.message == reflow::SET_WINDOW_BORDER
               || self.controls.is_root(call.id, owner)?)
        {
            if !self.controls.is_root(call.id, owner)?
            {
                return Err(BAD_ARGUMENT);
            }
            let viewport = self.windows.window(WindowId(window)).ok_or(NOT_FOUND)?.local_bounds();
            let pending =
                self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window));
            let prepared = self.controls.prepare_window(window,
                                                         viewport,
                                                         pending,
                                                         Some((call, owner)),
                                                         crate::machine::traps::gateway)?;
            let (used, limit) = self.retained_admission(SceneTarget::from(window));
            return self.publish_control_layout(window, prepared, used, limit, diagnostic);
        }
        if call.operation == REFLOW_OBJECT
           && matches!(call.layout.message, reflow::ASK_SIZE | reflow::ASSIGN_RECT)
           && self.controls.is_table(call.id, owner)?
        {
            let paint_pending =
                self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window));
            let (used, limit) = self.retained_admission(SceneTarget::from(window));
            let prepared = self.controls.prepare_layout(call,
                                                         owner,
                                                         paint_pending,
                                                         crate::machine::traps::gateway,
                                                         None)?;
            call.layout.measurement = prepared.measurement;
            self.publish_control_layout(window, prepared, used, limit, diagnostic)?;
            return Ok(());
        }
        if self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window))
           && self.controls.changes_tree_geometry(call, owner)?
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == DISPOSE_OBJECT
        {
            let split_before = self.controls.window_splitter_capture(window);
            self.controls.dispose(call.id, owner)?;
            if split_before && !self.controls.window_splitter_capture(window)
            {
                self.apply_control_outline(window, None)?;
                self.present();
            }
            self.remove_dead_control_drawings(window);
            return Ok(());
        }
        let split_before = !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
                           && self.controls.splitter_capture(call.id, owner)?;
        let entry = match self.controls.bound_definition(call, owner)?
        {
            Some(entry) => entry,
            None =>
            {
                let (id, _) = self.controls.preflight_definition(&recipient, &storage, call)?;
                crate::toolbox::runtime::Toolbox::control_definition(id)?
            }
        };
        let (used, limit) = self.retained_admission(SceneTarget::from(window));
        let content = self.presentation
                          .pending_content
                          .iter_mut()
                          .chain(&mut self.presentation.content)
                          .find(|content| content.id == SceneTarget::from(window));
        let drawing =
            self.controls.request_with(&recipient,
                                        &storage,
                                        call,
                                        entry,
                                        crate::machine::traps::gateway,
                                        |drawing| {
                                            if window == 0
                                            {
                                                return Err(BAD_ARGUMENT);
                                            }
                                            let content = content.ok_or(NOT_FOUND)?;
                                            let mut staging = Some(drawing);
                                            let (result, retired) =
                                                control_runtime::publish_drawing(content,
                                                                                 &mut staging,
                                                                                 used, limit)?;
                                            // Preserve the ordinary call's existing destruction
                                            // boundary.
                                            drop(retired);
                                            Ok(result)
                                        })?;
        if split_before && !self.controls.splitter_capture(call.id, owner)?
        {
            self.apply_control_outline(window, None)?;
            self.present();
        }
        if self.controls.in_tree(call.id)
           && (drawing.as_ref().is_some_and(|(bounds, _, old)| *old != Some(*bounds))
               || (call.operation == REFLOW_OBJECT
                   && call.layout.message == reflow::SET_VISIBLE_RECT))
        {
            self.sync_control_tree(window);
        }
        if let Some((bounds, damage, old_bounds)) = drawing
        {
            if let Some(old) = old_bounds
               && old != bounds
            {
                self.content_changed_rect(window, old);
            }
            for damage in damage.into_iter().filter(|r| !r.is_empty())
            {
                self.content_changed_rect(window, damage);
            }
            // A control state change commits its own pixels before returning.
            // Full window updates remain private until EndUpdate.
            if damage.iter().any(|r| !r.is_empty())
               && !self.presentation
                       .pending_content
                       .iter()
                       .any(|c| c.id == SceneTarget::from(window))
            {
                self.present();
            }
        }
        Ok(())
    }

    fn finish_managed_control(&mut self, call: &mac_abi::controls::ControlCall, window: u64,
                              drawing: Option<super::controls::Drawing>)
                              -> Result<(), Status>
    {
        use mac_abi::controls::*;
        if call.operation == DISPOSE_OBJECT
        {
            let mut removed = Vec::new();
            let mut coalesced = None;
            for content in self.presentation
                               .content
                               .iter_mut()
                               .chain(self.presentation.pending_content.iter_mut())
                               .filter(|c| c.id == SceneTarget::from(window))
            {
                content.retain_commands(|command| {
                           if let RenderCommand::Control(d) = command
                              && d.id == call.id
                           {
                               accumulate_removed_damage(&mut removed, &mut coalesced, d.bounds());
                               return false;
                           }
                           true
                       });
            }
            for bounds in removed.into_iter().chain(coalesced)
            {
                self.content_changed_rect(window, bounds);
            }
        }
        if let Some(drawing) = drawing
        {
            if window == 0
            {
                return Err(BAD_ARGUMENT);
            }
            let bounds = drawing.bounds();
            let damage = drawing.damage();
            let (used, limit) = self.retained_admission(SceneTarget::from(window));
            let size = drawing.storage();
            let content = self.content_mut(window)?;
            let old = content.iter_commands_mut()
                             .find(|c| matches!(c, RenderCommand::Control(d) if d.id == call.id));
            let old_bounds = if let Some(command) = old
            {
                if used - command.storage() + size > limit
                {
                    return Err(NO_MEMORY);
                }
                let old_bounds = if let RenderCommand::Control(d) = command
                {
                    d.bounds()
                }
                else
                {
                    unreachable!()
                };
                *command = RenderCommand::Control(drawing);
                Some(old_bounds)
            }
            else
            {
                if used + size > limit || content.commands.len() >= 512
                {
                    return Err(NO_MEMORY);
                }
                content.record(RenderCommand::Control(drawing))?;
                None
            };
            if let Some(old) = old_bounds
               && old != bounds
            {
                self.content_changed_rect(window, old);
            }
            for damage in damage.into_iter().filter(|r| !r.is_empty())
            {
                self.content_changed_rect(window, damage);
            }
            // A control state change commits its own pixels before returning.
            // Full window updates remain private until EndUpdate.
            if damage.iter().any(|r| !r.is_empty())
               && !self.presentation
                       .pending_content
                       .iter()
                       .any(|c| c.id == SceneTarget::from(window))
            {
                self.present();
            }
        }
        Ok(())
    }

    /// A retained object is already bound. Only a validated cache miss may
    /// ask the native coordinator to admit executable pages outside Desktop.
    pub(crate) fn preflight_control(&self, call: &mac_abi::controls::ControlCall)
                                    -> Result<Option<i16>, Status>
    {
        use mac_abi::controls::*;
        let id = if call.operation >= CREATE_STANDARD
        {
            let (recipient, storage) = self.service_owners()?;
            let owner = recipient.owner().incarnation();
            let bound = self.controls.bound_definition(call, owner)?;
            let window = if matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
            {
                call.window
            }
            else
            {
                self.controls.window(call.id, owner)?
            };
            if window != 0 && !self.owns(WindowId(window))
            {
                return Err(if self.windows.window(WindowId(window)).is_some()
                           {
                               ACCESS_DENIED
                           }
                           else
                           {
                               NOT_FOUND
                           });
            }
            if let Some(auxiliary) = self.controls.auxiliary_definition(call, owner)?
            {
                auxiliary
            }
            else if bound.is_some()
            {
                return Ok(None);
            }
            else
            {
                self.controls.preflight_definition(&recipient, &storage, call)?.0
            }
        }
        else
        {
            if !call.surface.is_null()
            {
                return Err(BAD_ARGUMENT);
            }
            if call.window != 0 && !self.owns(WindowId(call.window))
            {
                return Err(if self.windows.window(WindowId(call.window)).is_some()
                           {
                               ACCESS_DENIED
                           }
                           else
                           {
                               NOT_FOUND
                           });
            }
            if call.window == 0 && matches!(call.operation, DRAW_STANDARD | DRAW_SCROLLBAR)
            {
                return Err(NOT_FOUND);
            }
            super::controls::preflight(call)?
        };
        match crate::loader::cached_definition(*b"CDEF", id).map_err(crate::loader::status)?
        {
            Some(_) => Ok(None),
            None => Ok(Some(id))
        }
    }

    pub(crate) fn remove_dead_control_drawings(&mut self, window: u64)
    {
        let mut removed = Vec::new();
        let mut coalesced = None;
        for content in self.presentation
                           .content
                           .iter_mut()
                           .chain(self.presentation.pending_content.iter_mut())
                           .filter(|c| c.id == SceneTarget::from(window))
        {
            content.retain_commands(|command| {
                       if let RenderCommand::Control(d) = command
                          && d.id != 0
                          && !self.controls.live(d.id)
                       {
                           accumulate_removed_damage(&mut removed, &mut coalesced, d.bounds());
                           return false;
                       }
                       true
                   });
        }
        let changed = !removed.is_empty() || coalesced.is_some();
        for bounds in removed.into_iter().chain(coalesced)
        {
            self.content_changed_rect(window, bounds);
        }
        if changed
           && !self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window))
        {
            self.present();
        }
    }

    fn sync_control_tree(&mut self, window: u64)
    {
        let mut damage = Vec::new();
        let mut coalesced = None;
        if let Some(content) = self.presentation
                                   .pending_content
                                   .iter_mut()
                                   .chain(&mut self.presentation.content)
                                   .find(|c| c.id == SceneTarget::from(window))
        {
            content.sync_control_tree(&self.controls, |bounds| {
                       accumulate_removed_damage(&mut damage, &mut coalesced, bounds);
                   });
        }
        let changed = !damage.is_empty() || coalesced.is_some();
        for bounds in damage.into_iter().chain(coalesced)
        {
            self.content_changed_rect(window, bounds);
        }
        if changed
           && !self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(window))
        {
            self.present();
        }
    }

    pub(crate) fn control(&mut self, call: &mut mac_abi::controls::ControlCall,
                          diagnostic: &mut Option<TrackingStage>)
                          -> Status
    {
        if call.operation >= mac_abi::controls::CREATE_STANDARD
        {
            return self.managed_control(call, diagnostic).map_or_else(|status| TrackingStage::refusal(diagnostic, TrackingStage::ManagedAdmission, status), |_| OK);
        }
        if !call.surface.is_null() || call.reflow != 0
        {
            return BAD_ARGUMENT;
        }
        if call.window != 0 && self.windows.window(WindowId(call.window)).is_none()
        {
            return NOT_FOUND;
        }
        let id = match super::controls::preflight(call)
        {
            Ok(id) => id,
            Err(e) => return e
        };
        let entry = match crate::loader::cached_definition(*b"CDEF", id).and_then(|entry| {
                                                                            entry.ok_or_else(|| {
                                       mac_formats::Error::Missing("control definition was not \
                                                                    prepared before object \
                                                                    mutation").into()
                                   })
                                                                        })
        {
            Ok(entry) => entry,
            Err(e) => return crate::loader::status(e)
        };
        let drawing = match super::controls::prepare(call, entry, crate::machine::traps::gateway)
        {
            Ok(Some(drawing)) => drawing,
            Ok(None) => return OK,
            Err(e) => return e
        };
        if self.windows.window(WindowId(call.window)).is_none()
        {
            return NOT_FOUND;
        }
        let (used, limit) = self.retained_admission(SceneTarget::from(call.window));
        if used + drawing.storage() > limit
        {
            return NO_MEMORY;
        }
        let content = match self.content_mut(call.window)
        {
            Ok(c) => c,
            Err(e) => return e
        };
        let bounds = drawing.bounds();
        if let Err(status) = content.record(RenderCommand::Control(drawing))
        {
            return status;
        }
        self.content_changed_rect(call.window, bounds);
        OK
    }

    pub fn draw(&mut self, id: u64, command: DrawCommand) -> Status
    {
        let (used, limit) = self.retained_admission(SceneTarget::from(id));
        if used + core::mem::size_of::<DrawCommand>() > limit
        {
            return NO_MEMORY;
        }
        if command.kind > DRAW_BUTTON || command.text_length as usize > command.text.len()
           || core::str::from_utf8(&command.text[..(command.text_length as usize).min(command.text.len())]).is_err()
        {
            return BAD_ARGUMENT;
        }
        let content = match self.content_mut(id)
        {
            Ok(content) => content,
            Err(status) => return status
        };
        if content.commands.len() >= 512
        {
            return NO_MEMORY;
        }
        let command = match alloc::boxed::Box::try_new(command)
        {
            Ok(command) => command,
            Err(_) => return NO_MEMORY
        };
        if let Err(status) = content.record(RenderCommand::Legacy(command))
        {
            return status;
        }
        self.content_changed(id);
        OK
    }

    fn masked_update(&mut self, call: &mac_abi::quickdraw::GraphicsCall, bytes: &[u8])
                     -> Result<(), Status>
    {
        use mac_abi::quickdraw::*;

        use super::quickdraw::region::{BooleanOp, Region};
        let target = SceneTarget::decode(call.target)?;
        let id = target;
        // Use the ordinary owner/generation gate, including stale window IDs.
        if self.content_mut(target)?.commands.is_core()
        {
            return Err(NOT_SUPPORTED);
        }
        if matches!(call.operation, QD_WINDOW_BEGIN_MASK | QD_WINDOW_BEGIN_RECTS)
        {
            let commands = {
                let content = self.presentation
                                  .content
                                  .iter()
                                  .find(|content| content.id == id)
                                  .ok_or(NOT_FOUND)?;
                if content.has_report_commands()
                {
                    let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                    let window = id.window()?;
                    if !content.report_binding(&activity, window)
                    {
                        return Err(ACCESS_DENIED);
                    }
                    let (frame, lease) = activity.take_report_frame(window)?;
                    Some(super::quickdraw::retained::Commands::from_report(frame, lease))
                }
                else
                {
                    None
                }
            };
            let bounds = {
                let window = self.windows.window(id.window()?).ok_or(NOT_FOUND)?;
                if window.updating
                {
                    return Err(BAD_ARGUMENT);
                }
                window.local_bounds()
            };
            let mask = if call.operation == QD_WINDOW_BEGIN_RECTS
            {
                if bytes.is_empty() || !bytes.len().is_multiple_of(8) || bytes.len() > 1056 * 8
                {
                    return Err(BAD_ARGUMENT);
                }
                Region::from_rectangles(bytes.as_chunks::<8>().0.iter().map(|bytes| {
                    let at=|index| i16::from_le_bytes([bytes[index],bytes[index+1]]);
                    Rect::new(at(0),at(2),at(4),at(6))
                })).map_err(super::quickdraw::regions_service::status)?
            }
            else
            {
                Region::from_be_bytes(bytes).map_err(super::quickdraw::regions_service::status)?
            };
            let mask = mask.combine(&Region::from_rect(bounds), BooleanOp::Intersection)
                           .map_err(super::quickdraw::regions_service::status)?;
            if mask.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let spare = self.scene_spare();
            shared_scene_allocation(|| {
                self.presentation.pending_content.try_reserve(1 + spare)
            }).map_err(|_| NO_MEMORY)?;
            let content = self.presentation.content.iter().find(|c| c.id == id).ok_or(NOT_FOUND)?;
            let mut pending = Content::pending_patch(id, content.port.clone(), mask);
            if let Some(commands) = commands
            {
                pending.commands = commands;
            }
            let (used, limit) = self.retained_admission(id);
            if used.saturating_add(pending.storage()) > limit
            {
                return Err(NO_MEMORY);
            }
            if !self.windows.begin_partial_update(id.window()?)
            {
                return Err(BAD_ARGUMENT);
            }
            self.presentation.pending_content.push(pending);
            self.textedit.begin_cursor_update(id.window()?);
        }
        else
        {
            let index = self.presentation
                            .pending_content
                            .iter()
                            .position(|c| c.id == id && c.is_patch())
                            .ok_or(BAD_ARGUMENT)?;
            if call.operation == QD_WINDOW_CANCEL_MASK
            {
                self.presentation.pending_content.swap_remove(index);
                self.textedit.begin_cursor_update(id.window()?);
                self.windows.end_update(id.window()?);
            }
            else
            {
                let mut request = mac_abi::WindowRequest { id:             id.window()?.0,
                                                           bounds:         Rect::ZERO,
                                                           title:          [0; WINDOW_TITLE_BYTES],
                                                           title_length:   0,
                                                           procedure:      0,
                                                           minimum_width:  0,
                                                           minimum_height: 0,
                                                           reserved:       0 };
                let status = self.window(END_UPDATE, &mut request);
                if status != OK
                {
                    return Err(status);
                }
            }
        }
        Ok(())
    }

    pub fn graphics(&mut self, call: &mut mac_abi::quickdraw::GraphicsCall, points: &[Point],
                    region: &[u8])
                    -> Result<(), Status>
    {
        use mac_abi::quickdraw::QD_SCREEN_BOUNDS;
        if call.operation == mac_abi::quickdraw::QD_WINDOW_OUTLINE
        {
            if call.verb > 1
            {
                return Err(BAD_ARGUMENT);
            }
            self.content_mut(SceneTarget::decode(call.target)?)?;
            let next = (call.verb == 1 && !call.bounds.is_empty()).then_some(call.bounds);
            let target = SceneTarget::decode(call.target)?;
            #[cfg(feature = "event-trace")]
            {
                let r = call.bounds;
                let packed = u64::from(r.left as u16)
                             | (u64::from(r.top as u16) << 16)
                             | (u64::from(r.right as u16) << 32)
                             | (u64::from(r.bottom as u16) << 48);
                crate::trace::record(crate::trace::WINDOW,
                                     self.owner,
                                     crate::trace::CONTENT_OUTLINE_REQUEST,
                                     target.window()?.0,
                                     packed,
                                     u64::from(next.is_some()));
            }
            let id = target;
            if let Some(pending) =
                self.presentation.pending_content.iter_mut().find(|content| content.id == id)
            {
                pending.outline = Some(next);
            }
            else
            {
                self.apply_content_outline(target, next)?;
                self.present();
            }
            return Ok(());
        }
        if matches!(call.operation,
                    mac_abi::quickdraw::QD_WINDOW_BEGIN_MASK
                    | mac_abi::quickdraw::QD_WINDOW_BEGIN_RECTS
                    | mac_abi::quickdraw::QD_WINDOW_COMMIT_MASK
                    | mac_abi::quickdraw::QD_WINDOW_CANCEL_MASK)
        {
            return self.masked_update(call, region);
        }
        if call.operation == QD_SCREEN_BOUNDS
        {
            call.bounds = self.screen;
            return Ok(());
        }
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            return self.worlds.get_mut(world.0, self.owner)?.graphics(call, points, region);
        }
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        let saved = content.port.clone();
        if let Some(drawing) = service::graphics(&mut content.port, call, points, region)?
        {
            if used + drawing.storage() > limit
            {
                content.port = saved;
                return Err(NO_MEMORY);
            }
            if let Err(status) = content.record(RenderCommand::Graphics(drawing))
            {
                content.port = saved;
                return Err(status);
            }
            self.content_changed(SceneTarget::decode(call.target)?);
        }
        Ok(())
    }

    pub fn copy_bits(&mut self, call: &mac_abi::quickdraw::BitmapCall, data: &[u8],
                     palette: &[u32], mask: &[u8])
                     -> Result<(), Status>
    {
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            return self.worlds.get_mut(world.0, self.owner)?.bitmap(call, data, palette, mask);
        }
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        let drawing = super::quickdraw::bitmaps_service::BitmapDrawing::new(call,
                                                                            data,
                                                                            palette,
                                                                            mask,
                                                                            content.port.clone())?;
        if used + drawing.storage() + drawing.port.storage() > limit
        {
            return Err(NO_MEMORY);
        }
        content.record(RenderCommand::Bitmap(drawing))?;
        self.content_changed(SceneTarget::decode(call.target)?);
        Ok(())
    }

    pub fn cursor(&mut self, call: &mut mac_abi::quickdraw::CursorCall, colors: &[u32])
                  -> Result<(), Status>
    {
        let changed = self.pointer.cursors.request(call, colors, self.events.mouse)?;
        self.presentation.cursor_dirty |= changed && self.owner == self.foreground;
        Ok(())
    }

    pub fn cursor_resource(&mut self, call: &mut mac_abi::quickdraw::CursorResourceCall)
                           -> Result<(), Status>
    {
        let changed = crate::kernel::with_resources_in(call.scope, |resources| {
            self.pointer.cursors.resource_request(call,
                                          self.events.mouse,
                                          Self::when(),
                                          |kind, id, scope| {
                                              let (_,resource)=resources.get(scope,ResourceKey { kind,id })?;
                                              super::quickdraw::cursors::copy_resource(&resource.data)
                                          })
        })??;
        self.presentation.cursor_dirty |= changed && self.owner == self.foreground;
        Ok(())
    }

    /// The foreground application owns cursor policy; only an exposed editor
    /// content region can request an automatic I-beam. Retained window geometry
    /// excludes chrome/occlusion without author HIT on mouse motion; TextEdit
    /// accounts for its port clip. This advisory result never admits input.
    fn automatic_cursor_at_mouse(&self) -> Option<u32>
    {
        if !self.pointer.cursors.is_automatic()
           || self.menus.is_tracking()
           || self.windows.is_tracking()
        {
            return None;
        }
        let id = self.windows.active_window()?;
        if !self.owns(id)
        {
            return None;
        }
        let window = self.windows.window(id)?;
        let local = window.global_to_local(self.events.mouse);
        let hover = self.windows.cached_content_window(self.events.mouse) == Some(id);
        if let Some(axis) = self.controls.splitter_axis_at(id.0, local, hover)
        {
            return Some(if axis == 0 { 17 } else { 21 });
        }
        if !hover
        {
            return None;
        }
        (self.textedit.cursor_hit(id, local)
         || self.standard_file
                .active(self.owner)
                .and_then(|key| self.standard_file.get(self.owner, key).ok())
                .is_some_and(|d| {
                    d.window == id
                    && d.mode == mac_abi::standard_file::PUT
                    && d.sheet == super::standard_file::Sheet::Main
                    && d.editor.info().view.contains(local)
                })).then_some(1)
    }

    pub fn world(&mut self, call: &mut mac_abi::quickdraw::WorldCall, pixels: &mut [u32])
                 -> Result<(), Status>
    {
        use mac_abi::quickdraw::*;
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        call.target.decode().map_err(|_| BAD_ARGUMENT)?;
        if call.operation != WORLD_COPY
        {
            return self.worlds.request(&self.processes.ledger(self.owner)?, call, pixels);
        }
        let (bitmap, data) = self.worlds.get(call.world, self.owner)?.copy_request(call)?;
        self.copy_bits(&bitmap, &data, &[], &[])
    }

    pub fn font_draw(&mut self, call: &mut mac_abi::quickdraw::FontCall, text: &str,
                     fonts: &FontManager)
                     -> Result<(), Status>
    {
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            return self.worlds.get_mut(world.0, self.owner)?.text(call, text, fonts);
        }
        self.font_draw_retained(call, text, fonts, |content, text| {
                let mut retained_text = String::new();
                retained_text.try_reserve_exact(text.len()).map_err(|_| NO_MEMORY)?;
                retained_text.push_str(text);
                let port = Arc::try_new(content.port.clone()).map_err(|_| NO_MEMORY)?;
                Ok((super::quickdraw::retained::TextData::Owned(retained_text), port))
            })
    }

    fn font_draw_retained(&mut self, call: &mut mac_abi::quickdraw::FontCall, text: &str,
                          fonts: &FontManager,
                          prepare_text: impl FnOnce(&mut Content,
                                 &str)
                                 -> Result<(super::quickdraw::retained::TextData,
                                            Arc<service::Port>),
                                           Status>)
                          -> Result<(), Status>
    {
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        if content.commands.is_core()
        {
            // Core text must come from its typed SAME-frame snapshot seam.
            return Err(ACCESS_DENIED);
        }
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        if used + text.len() + content.port.storage() + 32 > limit
        {
            return Err(NO_MEMORY);
        }
        call.width = fonts.text_width(fonts_service::selection(call.selection), text)?;
        let (retained_text, port) = prepare_text(content, text)?;
        content.record(RenderCommand::Text { font: call.selection, baseline: call.baseline,
            text: retained_text, color: call.color, port,
            vertical_ink: fonts.vertical_ink_bounds(fonts_service::selection(call.selection)).ok().flatten() })?;
        self.content_changed(SceneTarget::decode(call.target)?);
        Ok(())
    }

    pub(super) fn capture_report_port(&self, window: WindowId)
                                      -> Result<crate::recovery::ReportPortView, Status>
    {
        let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
        let content = self.presentation
                          .pending_content
                          .iter()
                          .find(|c| c.id == window)
                          .ok_or(BAD_ARGUMENT)?;
        if self.crash.info().window != window || !content.report_binding(&activity, window)
        {
            return Err(ACCESS_DENIED);
        }
        content.commands.capture_report_port(&activity, window, &content.port)
    }

    pub(super) fn font_draw_report_port(&mut self, call: &mut mac_abi::quickdraw::FontCall,
                                        text: &str, fonts: &FontManager,
                                        view: &crate::recovery::ReportPortView)
                                        -> Result<(), Status>
    {
        let window = SceneTarget::decode(call.target)?.window()?;
        // Capture admitted the real live Session once per actual transition.
        // Each record checks only its pinned frame, Window and actual Port.
        self.font_draw_retained(call, text, fonts, |content, text| {
                content.commands.report_text(view, window, &content.port, text)
            })
    }

    pub(super) fn bind_core_scene(&mut self, window: WindowId,
                                  activity: &crate::recovery::Activity)
                                  -> Result<(), Status>
    {
        activity.validate_core_paint(window)?;
        if !self.owns(window)
           || self.presentation.pending_content.iter().any(|content| content.id == window)
        {
            return Err(ACCESS_DENIED);
        }
        let content = self.presentation
                          .content
                          .iter_mut()
                          .find(|content| content.id == window)
                          .ok_or(NOT_FOUND)?;
        if content.commands.is_core()
        {
            return if content.commands.core_binding(activity, window)
            {
                Ok(())
            }
            else
            {
                Err(ACCESS_DENIED)
            };
        }
        if !content.empty_for_recovery_binding()
        {
            return Err(BAD_ABI);
        }
        super::dialogs::core_alerts::bind_frames(activity, window)?;
        let (frame, lease) = activity.take_core_frame(window)?;
        // No fallible work follows the affine transfer into its actual owner.
        content.commands = super::quickdraw::retained::Commands::from_core(frame, lease);
        Ok(())
    }

    pub(super) fn bind_report_scene(&mut self, window: WindowId,
                                    activity: &crate::recovery::Activity)
                                    -> Result<(), Status>
    {
        activity.validate_report_paint(window)?;
        if !self.owns(window)
           || self.presentation.pending_content.iter().any(|content| content.id == window)
           || self.crash.info().window != window
           || self.crash.info().page_lines != super::crashes::prepared_layout()?.page_lines
        {
            return Err(ACCESS_DENIED);
        }
        let content = self.presentation
                          .content
                          .iter_mut()
                          .find(|content| content.id == window)
                          .ok_or(NOT_FOUND)?;
        if content.has_report_commands()
        {
            return if content.report_binding(activity, window)
            {
                Ok(())
            }
            else
            {
                Err(ACCESS_DENIED)
            };
        }
        if !content.empty_for_recovery_binding()
        {
            return Err(BAD_ABI);
        }
        activity.bind_report_frames(window)?;
        let (frame, lease) = activity.take_report_frame(window)?;
        // No fallible work follows minting this affine buffer owner.
        content.commands = super::quickdraw::retained::Commands::from_report(frame, lease);
        Ok(())
    }

    pub(super) fn font_draw_core_line(&mut self, call: &mut mac_abi::quickdraw::FontCall,
                                      item: &super::quickdraw::retained::CoreTextItem,
                                      range: core::ops::Range<usize>, fonts: &FontManager)
                                      -> Result<(), Status>
    {
        let window = SceneTarget::decode(call.target)?.window()?;
        let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
        // Item admission already validated the actual live Window and global
        // Session once. Each line checks its exact pinned frame/source/Port;
        // the original END_UPDATE still validates the current Window owner.
        let text = item.line(range.clone())?;
        call.width = fonts.text_width(fonts_service::selection(call.selection), text)?;
        let content = self.presentation
                          .pending_content
                          .iter_mut()
                          .find(|content| content.id == window)
                          .ok_or(BAD_ARGUMENT)?;
        content.commands.record_core_line(item, call, range,
            fonts.vertical_ink_bounds(fonts_service::selection(call.selection)).ok().flatten(),
            window, &content.port, &activity)?;
        // This is the same retained Text command/renderer; pending changes stay
        // private until the original END_UPDATE commits them.
        self.content_changed(window);
        Ok(())
    }

    pub fn picture(&mut self, call: &mac_abi::quickdraw::PictureCall,
                   mut drawing: super::quickdraw::pictures_service::Drawing, fonts: &FontManager)
                   -> Result<(), Status>
    {
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            let world = self.worlds.get_mut(world.0, self.owner)?;
            drawing.port = world.port.clone();
            return drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height),
                                 fonts);
        }
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > limit
        {
            return Err(NO_MEMORY);
        }
        content.record(RenderCommand::Picture(drawing))?;
        self.content_changed(SceneTarget::decode(call.target)?);
        Ok(())
    }

    pub fn image(&mut self, call: &mac_abi::images::ImageCall,
                 mut drawing: super::quickdraw::png::Drawing)
                 -> Result<(), Status>
    {
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            let world = self.worlds.get_mut(world.0, self.owner)?;
            drawing.port = world.port.clone();
            drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height));
            return Ok(());
        }
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > limit
        {
            return Err(NO_MEMORY);
        }
        content.record(RenderCommand::Image(drawing))?;
        self.content_changed(SceneTarget::decode(call.target)?);
        Ok(())
    }

    pub fn icon(&mut self, call: &mac_abi::quickdraw::IconCall,
                mut drawing: super::quickdraw::icons_service::Drawing)
                -> Result<(), Status>
    {
        if let mac_abi::quickdraw::DrawingTarget::World(world) =
            call.target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            let world = self.worlds.get_mut(world.0, self.owner)?;
            drawing.port = world.port.clone();
            return drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height));
        }
        let (used, limit) = self.retained_admission(SceneTarget::decode(call.target)?);
        let content = self.content_mut(SceneTarget::decode(call.target)?)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > limit
        {
            return Err(NO_MEMORY);
        }
        content.record(RenderCommand::Icon(drawing))?;
        self.content_changed(SceneTarget::decode(call.target)?);
        Ok(())
    }

    fn action(&mut self, action: Action, when: u32)
    {
        self.action_at_input(action, when, self.events.mouse, self.events.modifiers);
    }

    fn action_at_input(&mut self, action: Action, when: u32, point: Point, modifiers: u16)
    {
        let context = Event { where_: point,
                              modifiers,
                              ..self.events.null_event(when) };
        // Held drag motion changes only the frontbuffer outline. Accepted
        // geometry/close changes are the point where the real scene changes.
        self.presentation.redraw |= !matches!(action, Action::CloseRequest(_));
        if let Action::Drag(id) | Action::Resize(id) = action
        {
            self.events.post_system(Event { kind: EventKind::OsEvent,
                                            message: mac_abi::events::WINDOW_GEOMETRY_CHANGED,
                                            window: id.0,
                                            ..context });
        }
        self.last_action = match action
        {
            Action::Drag(id) =>
            {
                // Ordinary DragWindow activates only on accepted release;
                // Command-drag leaves the foreground application unchanged.
                if self.windows.active_window() == Some(id)
                   && let Some(owner) = self.window_owner(id)
                   && owner != self.foreground
                {
                    let _ = self.activate_process_window(owner, Some(id));
                }
                "DRAG"
            }
            Action::Resize(_) => "RESIZE",
            Action::CloseRequest(id) =>
            {
                self.events.post_system(Event { kind: EventKind::OsEvent,
                                                message: WINDOW_CLOSE_REQUEST,
                                                window: id.0,
                                                when,
                                                ..context });
                "CLOSE_REQUEST"
            }
            Action::Close(id, closed_owner) =>
            {
                if let Err(status) = self.remove_closed_window_content(id, when)
                {
                    serial!("WINDOW_CLOSE_UI_REFUSED window={} status={status}; child owners \
                             retained",
                            id.0);
                    self.last_action = "CLOSE_UI_REFUSED";
                    return;
                }
                self.events.post_system_for(closed_owner,
                                            Event { kind: EventKind::OsEvent,
                                                    message: 0x02000000,
                                                    window: id.0,
                                                    when,
                                                    ..context });
                // The trusted notice retained its owner before disposal;
                // routing does not depend on a second ownership directory.
                self.route_events();
                "CLOSE"
            }
        };
    }

    fn input(&mut self, input: Input, when: u32)
    {
        self.observe_window_input_intent(input);
        if self.capture_window_input_tail(input, when)
        {
            return;
        }
        let hit = match self.plan_physical_hit(input, when)
        {
            input_callbacks::HitAdmission::Deferred => return,
            input_callbacks::HitAdmission::Ready(hit) => hit
        };
        self.input_with_observation(input, when, hit, None);
    }

    fn input_with_observation(&mut self, input: Input, when: u32,
                              hit: Option<super::windows::Hit>,
                              observation: Option<input_callbacks::ObservedInput>)
    {
        let report = match observation
        {
            Some(observation) => self.system_report_observed_input(observation, hit),
            None => self.system_report_input(input, when, hit)
        };
        match report
        {
            Ok(true) => return,
            Err(status) =>
            {
                note_recovery_input_refusal(self.owner, status);
                return;
            }
            Ok(false) => ()
        }
        if let Some(activity) =
            crate::recovery::for_input(self.owner).filter(|activity| !activity.is_system_report())
        {
            let kind =
                crate::recovery::active(self.owner, crate::recovery::Kind::CoreAlert).is_some();
            if let Err(status) =
                activity.run(|| {
                            if kind
                            {
                                self.with_core_dialogs(|toolbox| {
                                        toolbox.input_observed_or_physical(input, when, hit,
                                                                           observation)
                                    });
                            }
                            else
                            {
                                self.input_observed_or_physical(input, when, hit, observation);
                            }
                        })
            {
                note_recovery_input_refusal(activity.owner(), status);
            }
        }
        else
        {
            self.input_observed_or_physical(input, when, hit, observation);
        }
    }

    fn input_observed_or_physical(&mut self, input: Input, when: u32,
                                  hit: Option<super::windows::Hit>,
                                  observation: Option<input_callbacks::ObservedInput>)
    {
        match observation
        {
            Some(observation) => self.route_observed_input(observation, hit),
            None => self.input_impl_with_hit(input, when, hit)
        }
    }

    fn input_impl_with_hit(&mut self, input: Input, when: u32,
                           admitted_hit: Option<super::windows::Hit>)
    {
        let observed = self.observe_physical_input(input, when);
        self.route_observed_input(observed, admitted_hit);
    }

    fn route_observed_input(&mut self, observation: input_callbacks::ObservedInput,
                            admitted_hit: Option<super::windows::Hit>)
    {
        let before = self.windows.tracking_visual();
        self.route_observed_input_inner(observation, admitted_hit);
        self.invalidate_tracking_feedback(before);
    }

    fn route_observed_input_inner(&mut self, observation: input_callbacks::ObservedInput,
                                  admitted_hit: Option<super::windows::Hit>)
    {
        let input_callbacks::ObservedInput { input,
                                             when,
                                             modifiers,
                                             key: observed } = observation;
        if !self.observe_drag_release(observation, admitted_hit)
        {
            return;
        }
        #[cfg(feature = "display-event-debug")]
        if let Input::Key { mac_code,
                            down: true,
                            repeat: false,
                            modifiers,
                            .. } = input
           && modifiers & (CMD_KEY | mac_abi::events::OPTION_KEY | mac_abi::events::SHIFT_KEY)
              == CMD_KEY | mac_abi::events::OPTION_KEY | mac_abi::events::SHIFT_KEY
           && mac_code == 0x0f
        {
            let full =
                Rect::new(0, 0, crate::display::width() as i16, crate::display::height() as i16);
            let reduced = Rect::new(0, 0, full.right * 3 / 4, full.bottom * 3 / 4);
            let bounds = if self.screen == full { reduced } else { full };
            match self.windows
                      .preflight_fit_to_screen(bounds, mac_abi::menus::MENU_BAR_HEIGHT as i16)
            {
                Ok(()) => self.display_changed(bounds),
                Err(status) => serial!("DISPLAY_DEBUG_FIT_REFUSED status={status}")
            }
            return;
        }
        let report_scope = !self.system_report_selected()
                           || crate::context::recovery_activity().is_some_and(|activity| {
                                                                     activity.is_system_report()
                                                                 });
        if report_scope
           && match admitted_hit
           {
               Some(hit) => self.crash_input_with_hit(input, when, Some(hit)),
               None => self.crash_input(input, when)
           }
        {
            return;
        }
        if self.standard_file_input(input, when)
        {
            return;
        }
        if self.dialogs.active()
        {
            self.dialog_input(input, when);
            return;
        }
        if self.drag_input_observed(input, when, admitted_hit, modifiers)
        {
            return;
        }
        if self.menu_feedback_input(input)
        {
            return;
        }
        if self.stage_tracking_release(observation)
        {
            return;
        }
        match input
        {
            Input::Move(point) =>
            {
                if self.menus.is_tracking()
                {
                    self.presentation.redraw |=
                        self.menus.hover(point, &mut self.system_bar, &mut self.app_menu);
                    return;
                }
                if let Some(action) = self.track_window(point, false, when)
                {
                    self.action(action, when);
                }
            }
            Input::Button { point,
                            down } =>
            {
                if down
                {
                    if self.menus.begin(point, &self.system_bar, &self.app_menu)
                    {
                        self.presentation.redraw = true;
                        return;
                    }
                    if (0..20).contains(&point.v)
                    {
                        // Blank/disabled titles consume the press without
                        // starting capture or activating the desktop behind it.
                        return;
                    }
                    let hit =
                        match admitted_hit.or_else(|| self.resolve_observed_press(observation))
                        {
                            Some(hit) => hit,
                            None => return
                        };
                    let active_before = self.windows.active_window();
                    let owner = hit.window
                                   .and_then(|id| self.window_owner(id))
                                   .unwrap_or(crate::context::finder_app().0);
                    if owner != self.foreground && hit.part != WindowPart::Drag
                    {
                        let _ = self.activate_process_window(owner, hit.window);
                    }

                    if let Some(id) = hit.window
                    {
                        let active = active_before == Some(id);
                        // DragWindow activates a background window only after a
                        // successful release; Command-drag preserves its plane.
                        if hit.part != WindowPart::Drag
                        {
                            self.windows.select_window(id, &mut self.events, when);
                        }
                        if matches!(hit.part,
                                    WindowPart::Drag
                                    | WindowPart::Grow
                                    | WindowPart::GoAway
                                    | WindowPart::ZoomIn
                                    | WindowPart::ZoomOut)
                        {
                            self.windows.start_tracking_with_modifiers(hit, point, modifiers);
                        }
                        else if hit.part == WindowPart::Content
                                  && (active
                                      || self.windows.window(id).is_some_and(|w| w.stays_in_back()))
                        {
                            let local = self.windows.window(id).unwrap().global_to_local(point);
                            let button = self.presentation
                                             .content
                                             .iter()
                                             .find(|c| c.id == id)
                                             .and_then(|c| {
                                                 c.iter_commands()
                                                  .filter_map(|command| {
                                                      match command
                                                      {
                                                          RenderCommand::Legacy(c) => Some(c),
                                                          _ => None
                                                      }
                                                  })
                                                  .find(|command| {
                                                      command.kind == DRAW_BUTTON
                                                      && command.bounds.contains(local)
                                                  })
                                             });
                            if let Some(command) = button
                            {
                                let mut state = control(command);
                                let mut call = DefinitionCall { reflow: 0, header: Header::for_type::<DefinitionCall>(), kind: u32::from_be_bytes(*b"CDEF"), id: 0, variation: 0, message: 1, reserved: 0, param: (((local.v as u16 as u32) << 16) | local.h as u16 as u32) as i64, object: (&mut state as *mut crate::toolbox::controls::ControlDefinitionState).cast(), gate: crate::machine::traps::gateway, surface: core::ptr::null_mut() };
                                if super::resources::definitions::invoke(self.cdef, &mut call) > 0
                                {
                                    self.events.post_system(Event { payload: 0,
                                                                    kind: EventKind::OsEvent,
                                                                    message: 0x01000000,
                                                                    window: id.0,
                                                                    when,
                                                                    where_: point,
                                                                    modifiers: self.events
                                                                                   .modifiers });
                                }
                            }
                            self.drag.observe_pointer_press(self.owner, id.0, point, modifiers);
                            self.events.post_system(Event { payload: 0,
                                                            kind: EventKind::MouseDown,
                                                            message: 0,
                                                            window: id.0,
                                                            when,
                                                            where_: point,
                                                            modifiers });
                            self.last_action = "CONTENT";
                        }
                        else
                        {
                            self.last_action = "SELECT";
                        }
                    }
                    else
                    {
                        self.drag.observe_pointer_press(self.owner, 0, point, modifiers);
                        self.events.post_system(Event { payload: 0,
                                                        kind: EventKind::MouseDown,
                                                        message: 0,
                                                        when,
                                                        where_: point,
                                                        modifiers,
                                                        window: 0 });
                    }
                }
                else
                {
                    if self.menus.is_tracking()
                    {
                        if let Some(command) =
                            self.menus
                                .release_tracking(point, &mut self.system_bar, &mut self.app_menu)
                        {
                            self.start_menu_command_feedback(command, false, when);
                        }
                        else
                        {
                            self.presentation.redraw = true;
                        }
                        return;
                    }
                    if let Some(action) = self.track_window(point, true, when)
                    {
                        self.action(action, when);
                    }
                    self.events.post_system(Event { payload: 0,
                                                    kind: EventKind::MouseUp,
                                                    message: 0,
                                                    when,
                                                    where_: point,
                                                    modifiers,
                                                    window: 0 });
                }
            }
            Input::Key { character,
                         down,
                         modifiers,
                         .. } =>
            {
                if down && character == 27 && self.menus.is_tracking()
                {
                    self.menus.cancel_tracking();
                    self.presentation.redraw = true;
                    return;
                }
                if down
                   && modifiers & CMD_KEY != 0
                   && self.menus.has_key(character)
                   && let Some(selection) = self.menus.key(character)
                {
                    self.start_menu_key_feedback(selection, when);
                    return;
                }
                // Menu policy can still reflect the previous frame while the
                // application opens or closes a window. Disabled shortcuts stay
                // available as keyboard input; only a real selection consumes it.
                if let Some(event) = observed
                {
                    if down
                    {
                        self.last_key = character;
                    }
                    self.events.post_system(event);
                }
            }
        }
    }

    pub fn next(&mut self, mask: u16) -> Event
    {
        self.next_with_remove(mask, true)
    }

    /// A posted null event is a real queued record, distinct from idle. Keep
    /// availability separate so a native wait cannot consume it and then sleep.
    pub fn next_available(&mut self, mask: u16) -> (Event, bool)
    {
        self.next_with_presence(mask, true)
    }

    /// A genuine source/canvas return can unblock the retained settled mode.
    /// This only arms the existing video service; no geometry or I/O runs here.
    pub(crate) fn window_resources_returned(&mut self)
    {
        self.presentation.ordinary_underlay.source_returned();
        self.presentation.tracking_grow.source_returned();
        if self.presentation.pending_video_mode.is_some()
        {
            self.presentation.video_mode_wait = false;
        }
    }

    /// Presents video damage and applies a settled host size. Returns true when
    /// the display mode changed. Driver refusals leave the current mode alone.
    fn service_video(&mut self) -> bool
    {
        use mac_video_interface::{Outcome, Size};

        match crate::machine::video::service()
        {
            Ok(Some(size)) =>
            {
                self.presentation.pending_video_mode = Some(size);
                self.presentation.video_mode_wait = false;
            }
            Ok(None) => (),
            Err(status) =>
            {
                serial!("VIDEO_SERVICE_FAILED status={status}");
                return false;
            }
        };
        let Some(settled) = self.presentation.pending_video_mode
        else
        {
            return false;
        };
        if self.presentation.video_mode_wait
        {
            return false;
        }
        let width =
            (settled.width as usize).clamp(crate::display::MIN_WIDTH, crate::display::MAX_WIDTH);
        let height =
            (settled.height as usize).clamp(crate::display::MIN_HEIGHT, crate::display::MAX_HEIGHT);
        if width == crate::display::width() && height == crate::display::height()
        {
            self.presentation.pending_video_mode = None;
            self.presentation.video_mode_wait = false;
            return false;
        }
        // SERVICE reports a settled size once. Retain that actual request on
        // refusal and retry after a real source/canvas return, never each input.
        // Arm before preflight so a racing return cannot be discarded later.
        self.presentation.video_mode_wait = true;
        let bounds = Rect::new(0, 0, width as i16, height as i16);
        if let Err(status) =
            self.windows.preflight_fit_to_screen(bounds, mac_abi::menus::MENU_BAR_HEIGHT as i16)
        {
            if status != SERVICE_BUSY
            {
                serial!("VIDEO_MODE_ADMISSION_REFUSED {}x{}: window geometry refused; \
                         status={status}",
                        width,
                        height);
            }
            return false;
        }
        let size = Size { width:  width as u32,
                          height: height as u32 };
        let pixel_count = width * height;
        // These existing compositor Vecs are System ordinary backing. Admit
        // their new capacities before the device can change scanout. Keep the
        // actual mutable borrow until commit; failure preserves the old scene.
        let prepared = match crate::context::without_recovery(|| {
                  crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                      self.presentation.ordinary_underlay.prepare_mode(pixel_count)
                  })
              })
        {
            Ok(prepared) => prepared,
            Err(status) =>
            {
                serial!("VIDEO_MODE_ADMISSION_REFUSED {}x{}: compositor memory could not be \
                         prepared; status={status}",
                        width,
                        height);
                return false;
            }
        };
        let menu_layout = match crate::context::without_recovery(|| {
                  crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                      self.menus
                          .prepare_display(&mut self.system_bar, &mut self.app_menu, width as i16)
                  })
              })
        {
            Ok(layout) => layout,
            Err(status) =>
            {
                serial!("VIDEO_MODE_ADMISSION_REFUSED {}x{}: menu geometry could not be \
                         prepared; status={status}",
                        width,
                        height);
                return false;
            }
        };
        match crate::machine::video::apply(size)
        {
            Ok(Outcome::Applied) => (),
            Ok(refusal) =>
            {
                serial!("VIDEO_MODE_REFUSED {}x{}: {}", width, height, refusal.message());
                return false;
            }
            Err(status) =>
            {
                serial!("VIDEO_MODE_APPLY_FAILED {}x{} status={status}", width, height);
                return false;
            }
        }
        prepared.commit();
        menu_layout.commit();
        self.presentation.pending_video_mode = None;
        self.presentation.video_mode_wait = false;
        self.presentation.menu_cache.invalidate();
        serial!("DISPLAY_MODE_CHANGED {}x{}", width, height);
        self.display_changed(mac_abi::geometry::Rect::new(0, 0, width as i16, height as i16));
        true
    }

    fn poll_inputs(&mut self) -> bool
    {
        crate::trace::record(crate::trace::POLL, self.owner, 0, 0, 0, 0);
        let mut pending = [None; 128];
        let mut count = 0;
        virtio_input::poll(|device, kind, code, value| {
            self.decoders[device].feed(kind, code, value, |input| {
                                     if count < pending.len()
                                     {
                                         pending[count] = Some(input);
                                         count += 1;
                                     }
                                 })
        });
        let resized = self.service_video();
        // Idle polling must not copy every application's menus and event queues
        // into and out of the foreground context. Decoder state changes that
        // affect applications (including modifiers) always produce an Input.
        if count == 0
        {
            return resized;
        }
        let when = Self::when();
        let caller = self.owner;
        self.enter(self.foreground);
        for input in pending[..count].iter().flatten()
        {
            self.input(*input, when);
            self.route_events();
        }
        let mut keys = events::KeyMap::default();
        for decoder in &self.decoders
        {
            for (target, source) in keys.0.iter_mut().zip(decoder.key_map().0)
            {
                *target |= source;
            }
        }
        self.events.keys = keys;
        self.sync_input_state();
        self.enter(caller);
        pending[..count].iter()
                        .flatten()
                        .any(|input| matches!(input, Input::Move(_) | Input::Button { .. }))
    }

    fn next_with_remove(&mut self, mask: u16, remove: bool) -> Event
    {
        self.next_with_presence(mask, remove).0
    }

    fn next_with_presence(&mut self, mask: u16, remove: bool) -> (Event, bool)
    {
        let (event, _, available) = self.next_positioned(mask, remove, false);
        (event, available)
    }

    pub(super) fn next_positioned(&mut self, mask: u16, remove: bool, positioned: bool)
                                  -> (Event, mac_abi::events::EventPosition, bool)
    {
        self.presentation.cursor_dirty |=
            self.pointer.responsiveness.foreground(self.foreground, Self::when());
        self.presentation.cursor_dirty |=
            self.pointer.responsiveness.polled(self.owner, Self::when());
        self.poll_inputs();
        self.automatic_control_layout();
        let when = Self::when();
        self.route_events();
        let mut synthetic = false;
        let event =
            self.events.positioned_matching(mask, remove).or_else(|| {
                  synthetic = true;
                  let event = if mask & (1 << EventKind::Update as u16) != 0
                  {
                      if self.dialogs.active()
                      {
                          self.windows
                              .next_update_for(WindowId(self.dialogs.window), &self.events, when)
                      }
                      else
                      {
                          self.windows.next_update_matching(&self.events, when, |id| self.owns(id))
                      }
                  }
                  else
                  {
                      None
                  };
                  event.map(|event| (event, if positioned { self.event_position(event, self.owner) } else { Default::default() }))
              });
        let mut available = event.is_some();
        let (mut event, mut position) = event.unwrap_or_else(|| {
                                                 let event = self.events.null_event(when);
                                                 (event,
                                                  if positioned
                                                  {
                                                      self.event_position(event, self.owner)
                                                  }
                                                  else
                                                  {
                                                      Default::default()
                                                  })
                                             });
        if self.dialogs.system_menu && event.window == self.dialogs.window
        {
            if event.kind == EventKind::Update
            {
                if let Err(status) = self.service_system_menu_dialog(event)
                {
                    serial!("ABOUT_SYSTEM_PAINT_FAILED status={status}; protocol released or \
                             owned cleanup retained");
                }
            }
            event = self.events.null_event(when);
            position = Default::default();
            available = false;
        }
        // Disposal/reuse never transfers a queued coordinate to another object.
        if position.window != 0
           && (!self.owns(WindowId(position.window))
               || self.windows.window(WindowId(position.window)).is_none())
        {
            position = Default::default();
        }
        // Input delivery must not wait for painting. EndUpdate commits drawing;
        // an idle poll also presents pointer/menu changes and legacy drawing.
        if !available
        {
            self.present();
        }
        let code = if !available
        {
            crate::trace::NULL
        }
        else if !remove
        {
            crate::trace::PEEK
        }
        else if synthetic
        {
            crate::trace::SYNTH_UPDATE
        }
        else
        {
            crate::trace::DELIVERY
        };
        crate::trace::record(code, self.owner, event.kind as u64, event.message as u64,
                             event.when as u64, event.modifiers as u64);
        (event, position, available)
    }

    pub fn present(&mut self)
    {
        if self.system_report_temporary
        {
            self.system_report_present_pending = true;
            return;
        }
        self.presentation.redraw |= self.windows.take_selection_redraw();
        while let Some((id, region)) = self.windows.pop_selection_damage()
        {
            if self.windows.window(id).is_some()
            {
                self.queue_scene_damage(id.into(), region);
            }
        }
        crate::trace::record(crate::trace::PRESENT, self.owner, 0, 0, 0, 0);
        self.tick_menu_feedback();
        self.tick_cursor();
        let available = if self.presentation.redraw
                           || !self.presentation.content_damage.is_empty()
                           || self.presentation.ordinary_underlay.pending()
        {
            crate::kernel::heap_available()
        }
        else
        {
            0
        };
        let retry_ordinary = self.presentation.ordinary_underlay.retry_due(available);
        if !self.presentation.redraw
           && !self.presentation.cursor_dirty
           && self.presentation.content_damage.is_empty()
           && !self.presentation.feedback_dirty
           && self.presentation.presentation_damage.is_empty()
           && !self.presentation.presentation_copy_full
           && !retry_ordinary
        {
            self.publish_observation();
            return;
        }
        let caller = self.owner;
        if self.processes.ledger(self.foreground).is_ok_and(|owner| owner.accepting())
        {
            self.enter(self.foreground);
        }
        let full = self.presentation.redraw;
        let mut damaged = core::mem::take(&mut self.presentation.presentation_damage);
        let mut copy_full = core::mem::take(&mut self.presentation.presentation_copy_full);
        let compose = full || !self.presentation.content_damage.is_empty() || retry_ordinary;
        if compose
        {
            // Cursor/feedback-only presentation must leave the admitted damage
            // queue and its capacity in place. Every take below has a restore.
            let mut damage = core::mem::take(&mut self.presentation.content_damage);
            let ordinary_damage = damage.iter().any(|(id, _)| !self.system_scene(*id));
            let system_damage = damage.iter().any(|(id, _)| self.system_scene(*id));
            let mut scene_changed = full || self.presentation.frame_incomplete || system_damage;
            let started = crate::trace::clock();
            self.presentation.compositions = self.presentation.compositions.wrapping_add(1);
            let cache_budget = super::quickdraw::content_cache::PIXEL_BUDGET
                .saturating_sub(self.scroll_boxes.raster_storage())
                .saturating_sub(super::quickdraw::menu_cache::BYTES);
            self.presentation
                .content_cache
                .preserve_headroom(available, 2 * 1024 * 1024, cache_budget);
            if full || ordinary_damage || retry_ordinary
            {
                // Refusal retains the exact owned ordinary regions. The
                // committed base stays available for genuine System rendering.
                scene_changed |= self.update_ordinary_underlay(&mut damage, full, available,
                                                               &mut damaged, &mut copy_full)
                                     .is_ok_and(|changed| changed);
            }
            let rendered = if full || self.presentation.frame_incomplete
            {
                self.paint_system_layer(&mut damaged, &mut copy_full)
            }
            else
            {
                self.paint_system_damage(&damage, &mut damaged, &mut copy_full)
            };
            if rendered.is_err()
            {
                self.presentation.content_damage = damage;
                self.presentation.frame_incomplete = true;
                self.restore_presentation_damage(damaged, copy_full);
                if self.processes.ledger(caller).is_ok_and(|owner| owner.accepting())
                {
                    self.enter(caller);
                }
                return;
            }
            // A metadata refusal may leave an exact ordinary entry active.
            damage.retain(|(id, _)| !self.system_scene(*id));
            self.presentation.content_damage = damage;
            if full || self.menus.is_tracking()
            {
                scene_changed = true;
                let menu_paint = crate::context::without_recovery(|| {
                    let mut canvas = Canvas::new(platform::back_buffer(),
                                                 crate::display::width(),
                                                 crate::display::height());
                    self.presentation.menu_cache.paint(&mut canvas,
                                                        (self.foreground,
                                                         self.menus.selected_menu(),
                                                         self.menus.application_selected(),
                                                         self.menus.system_selected()),
                                                        |canvas| {
                                                            self.menus
                                                                .draw_bar(&mut self.system_bar,
                                                                          &mut self.app_menu,
                                                                          canvas)
                                                        })?;
                    self.menus.draw_popup(&mut self.system_bar, &mut self.app_menu, &mut canvas)
                });
                if let Err(error) = menu_paint
                {
                    if !self.presentation.frame_incomplete
                    {
                        serial!("MENU_PAINT_FAILED menu={} status={}; incomplete frame deferred",
                                error.menu, error.status);
                    }
                    self.presentation.redraw = true;
                    self.presentation.frame_incomplete = true;
                    self.restore_presentation_damage(damaged, copy_full);
                    if self.processes.ledger(caller).is_ok_and(|owner| owner.accepting())
                    {
                        self.enter(caller);
                    }
                    return;
                }
            }
            self.presentation.redraw = false;
            self.presentation.frame_incomplete = false;

            if scene_changed
            {
                // A waiting author stage has not published ordinary pixels.
                // Readiness alone must not advance the visible scene stamp.
                self.revision = self.revision.wrapping_add(1);
            }
            crate::trace::record(crate::trace::RENDER,
                                 caller,
                                 started,
                                 crate::trace::clock().wrapping_sub(started),
                                 0,
                                 self.revision as u64);
        }
        if full
           || copy_full
           || self.presentation.cursor_dirty
           || self.presentation.feedback_dirty
           || !damaged.is_empty()
        {
            self.compose_cursor(full, damaged, copy_full);
        }
        else
        {
            self.restore_presentation_damage(damaged, false);
        }
        self.publish_observation();
        if self.processes.ledger(caller).is_ok_and(|owner| owner.accepting())
        {
            self.enter(caller);
        }
    }

    /// Timer-owned presentation never enters application drawing or replays a
    /// scene. It remains available when every app thread is CPU bound.
    fn present_cursor_overlay(&mut self)
    {
        if self.system_report_temporary
        {
            self.system_report_cursor_pending = true;
            return;
        }
        // A refused scene replay may have touched only the private back buffer.
        // Never leak those uncommitted pixels through a later cursor-only copy.
        if self.presentation.frame_incomplete
           || !self.presentation.cursor_dirty
              && !self.presentation.feedback_dirty
              && self.presentation.presentation_damage.is_empty()
              && !self.presentation.presentation_copy_full
        {
            return;
        }
        let caller = self.owner;
        self.enter(self.foreground);
        let damage = core::mem::take(&mut self.presentation.presentation_damage);
        let copy_full = core::mem::take(&mut self.presentation.presentation_copy_full);
        self.compose_cursor(false, damage, copy_full);
        self.publish_observation();
        self.enter(caller);
    }

    fn restore_presentation_damage(&mut self, mut damaged: Vec<Rect>, copy_full: bool)
    {
        self.presentation.presentation_copy_full |= copy_full;
        if self.presentation.presentation_damage.is_empty()
        {
            core::mem::swap(&mut self.presentation.presentation_damage, &mut damaged);
        }
        else
        {
            for rect in damaged
            {
                accumulate_presentation_damage(&mut self.presentation.presentation_damage,
                                               &mut self.presentation.presentation_copy_full, rect);
            }
        }
    }

    fn prepare_feedback(&mut self, full: bool) -> super::quickdraw::feedback::Feedback
    {
        use super::quickdraw::{feedback::{Clip, ClippedOutline, Feedback, Highlight},
                               presentation::Outline};
        let grow = self.presentation.tracking_grow.overlay();
        // Preserve the ordinary feedback geometry while a scene is pending,
        // but release/cancel must still erase its actual accepted grow mask.
        if !full && self.presentation.redraw
        {
            let mut feedback = self.presentation.feedback.clone();
            feedback.grow = grow;
            return feedback;
        }
        if grow.is_some()
        {
            return Feedback { grow,
                              ..Feedback::default() };
        }
        if full
        {
            self.presentation.outline_clip.invalidate();
            self.presentation.highlight_clip.invalidate();
        }
        if let Some(shape) = self.windows.drag_outline()
        {
            return Feedback::outline(shape, Clip::Screen);
        }
        if self.menus.is_tracking() || self.windows.is_tracking()
        {
            return Feedback::default();
        }
        let mut feedback = Feedback::default();
        if let Some((window, bounds)) = self.drag.highlight()
        {
            let target = (window != 0).then_some(WindowId(window));
            feedback.highlight = Some(Highlight { bounds,
                                                  clip: self.presentation
                                                            .highlight_clip
                                                            .get(&self.windows, target) });
        }
        if let Some(bounds) = self.drag.feedback()
        {
            feedback.outline = Outline::new(&[bounds], 0, 0).map(|shape| {
                                                                ClippedOutline { shape,
                                                                                 clip:
                                                                                     Clip::Screen }
                                                            });
        }
        else if let Some(window) =
            self.windows.active_window().and_then(|id| self.windows.window(id))
        {
            if let Some(bounds) = window.outline_rect()
            {
                let shape = Outline::new(&[bounds],
                                         i32::from(window.bounds.left),
                                         i32::from(window.bounds.top));
                let clip = self.presentation.outline_clip.get(&self.windows, Some(window.id));
                feedback.outline = shape.map(|shape| {
                                            ClippedOutline { shape,
                                                             clip }
                                        });
            }
        }
        feedback
    }

    fn compose_cursor(&mut self, full: bool, mut damaged: Vec<Rect>, copy_full: bool)
    {
        let feedback = self.prepare_feedback(full);
        let report_cursor = self.report_cursor_is_front();
        let busy = self.pointer.responsiveness.active() && !report_cursor;
        let automatic = if report_cursor || busy { None } else { self.automatic_cursor_at_mouse() };
        let cursor_changed = self.presentation.cursor_dirty
                             || self.pointer.automatic_cursor != automatic
                             || self.pointer.report_cursor_visible != report_cursor;
        self.pointer.automatic_cursor = automatic;
        self.pointer.report_cursor_visible = report_cursor;
        let text_cursor = automatic.map(|id| {
                                       match id
                                       {
                                           17 => self.pointer.resize_cursors[0],
                                           21 => self.pointer.resize_cursors[1],
                                           _ => self.pointer.text_cursor
                                       }
                                   });
        // A System-owned report remains interactive after its author stops.
        // Use an independent visible arrow, preserving the app's actual cursor
        // policy, hidden state and responsiveness for restoration afterwards.
        let cursor = if report_cursor
        {
            &mut self.pointer.report_cursor
        }
        else if busy
        {
            &mut self.pointer.busy_cursor
        }
        else
        {
            &mut self.pointer.cursors
        };
        let text_cursor = if report_cursor || busy { None } else { text_cursor };
        let sprite = cursor.sprite(self.events.mouse, text_cursor);
        let bounds = sprite.map_or(Rect::ZERO, |sprite| sprite.bounds());
        let overlays =
            super::quickdraw::presentation::Overlays { previous_cursor: self.presentation
                                                                            .cursor_bounds,
                                                       cursor: sprite,
                                                       cursor_changed,
                                                       previous_feedback:
                                                           Some(&self.presentation.feedback),
                                                       feedback: Some(&feedback) };
        let back = platform::back_buffer();
        if full || copy_full
        {
            platform::present_rects(back,
                                    [Rect::new(0,
                                               0,
                                               crate::display::width() as i16,
                                               crate::display::height() as i16)],
                                    overlays);
        }
        else
        {
            platform::present_rects(back, damaged.iter().copied(), overlays);
        }
        self.acknowledge_menu_feedback();
        self.presentation.feedback = feedback;
        // Only after present-time transition stopped reading the previous mask
        // may its same backing be reused or its original account be refunded.
        self.presentation.tracking_grow.erased();
        self.presentation.feedback_dirty = !full && self.presentation.redraw;
        if self.presentation.presentation_damage.is_empty()
           && self.presentation.presentation_damage.capacity() < damaged.capacity()
        {
            damaged.clear();
            self.presentation.presentation_damage = damaged;
        }
        self.presentation.cursor_bounds = bounds;
        self.presentation.cursor_dirty = false;
        self.presentation.presentations = self.presentation.presentations.wrapping_add(1);
    }

    fn publish_observation(&mut self)
    {
        if self.last_observed == (self.activity, self.revision, self.presentation.presentations)
        {
            return;
        }
        let r = self.windows.window(self.first_window).map_or(Rect::ZERO, |w| w.bounds);
        let pair = |a: i16, b: i16| u64::from(a as u16) | (u64::from(b as u16) << 16);
        let mut action = [0; 16];
        let name = self.last_action.as_bytes();
        action[..name.len()].copy_from_slice(name);
        super::quickdraw::observer::publish([self.activity,
                                             u64::from(self.revision),
                                             pair(self.events.mouse.h, self.events.mouse.v),
                                             self.windows.count() as u64,
                                             u64::from(self.windows
                                                           .front_window()
                                                           .unwrap_or_default()
                                                           .0),
                                             u64::from(self.last_key),
                                             pair(r.left, r.top),
                                             pair(r.right, r.bottom),
                                             u64::from(self.updates),
                                             u64::from(self.events.dropped),
                                             u64::from_le_bytes(action[..8].try_into().unwrap()),
                                             u64::from_le_bytes(action[8..].try_into().unwrap()),
                                             self.presentation.menu_cache.paints,
                                             self.presentation.presentations,
                                             self.presentation.compositions]);
        self.last_observed = (self.activity, self.revision, self.presentation.presentations);
    }
}

fn paint(canvas: &mut Canvas, command: &DrawCommand, cdef: DefinitionRef)
{
    let text = core::str::from_utf8(&command.text[..command.text_length as usize]).unwrap_or("");
    match command.kind
    {
        PAINT_RECT => canvas.paint_rect(command.bounds, command.color),
        FRAME_RECT => canvas.frame_rect(command.bounds, command.color),
        DRAW_TEXT =>
        {
            GENEVA_9.draw(canvas, command.bounds.left, command.bounds.top + 10, text, command.color)
        }
        DRAW_BUTTON =>
        {
            let mut state = control(command);
            let mut surface = Surface::from_canvas(canvas);
            let mut call = DefinitionCall { reflow: 0, header: Header::for_type::<DefinitionCall>(), kind: u32::from_be_bytes(*b"CDEF"), id: 0, variation: 0, message: 0, reserved: 0, param: 0, object: (&mut state as *mut crate::toolbox::controls::ControlDefinitionState).cast(), gate: crate::machine::traps::gateway, surface: &mut surface };
            super::resources::definitions::invoke(cdef, &mut call);
        }
        _ =>
        {}
    }
}

fn control(command: &DrawCommand) -> crate::toolbox::controls::ControlDefinitionState
{
    let mut title = [0; mac_abi::INLINE_STRING_BYTES];
    let length = (command.text_length as usize).min(title.len());
    title[..length].copy_from_slice(&command.text[..length]);
    crate::toolbox::controls::ControlDefinitionState { bounds: command.bounds,
                                                       title,
                                                       title_length: length as u32,
                                                       enabled: 1,
                                                       highlighted: 0,
                                                       ..Default::default() }
}

fn background(canvas: &mut Canvas)
{
    let clip = canvas.clip();
    if let Some(mut raster) =
        mac_boot_screen::Raster::new(canvas.pixels, canvas.width, canvas.height)
    {
        raster.background_rect(clip);
    }
}

impl super::classic::Services for Toolbox
{
    fn drag(&mut self, call: &mut mac_abi::drag::Call, input: &[u8], output: &mut [u8])
            -> Result<(), Status>
    {
        self.drag_request(call, input, output)
    }

    fn event(&mut self, mask: u16, remove: bool) -> Result<Event, Status>
    {
        Ok(self.next_with_remove(mask, remove))
    }

    fn font_selection(&mut self, set: Option<mac_abi::quickdraw::FontSelection>)
                      -> Result<mac_abi::quickdraw::FontSelection, Status>
    {
        if let Some(selection) = set
        {
            crate::kernel::fonts().ok_or(NOT_SUPPORTED)?
                                  .metrics(fonts_service::selection(selection))?;
            self.classic_font = selection;
        }
        Ok(self.classic_font)
    }

    fn font(&mut self, operation: u32, selection: mac_abi::quickdraw::FontSelection, text: &str)
            -> Result<mac_abi::quickdraw::FontCall, Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let mut call = mac_abi::quickdraw::FontCall { operation,
                                                      selection,
                                                      ..Default::default() };
        let status = fonts_service::query(&fonts, &mut call, text);
        if status != OK
        {
            return Err(status);
        }
        Ok(call)
    }

    fn text(&mut self, text: &str) -> Result<(), Status>
    {
        let id = self.windows.front_window().ok_or(NOT_FOUND)?;
        let selection = self.classic_font;
        let content = self.content_mut(id.0)?;
        let mut call =
            mac_abi::quickdraw::FontCall { operation: mac_abi::quickdraw::FONT_DRAW,
                                           target:
                                               mac_abi::quickdraw::Target::window(WindowId(id.0)),
                                           selection,
                                           baseline: content.port.state.pen_location,
                                           color: content.port.state.foreground,
                                           ..Default::default() };
        self.font_draw(&mut call, text, crate::kernel::fonts().ok_or(NOT_SUPPORTED)?.as_ref())?;
        let content = self.content_mut(id.0)?;
        content.port.state.pen_location.h =
            content.port.state.pen_location.h.wrapping_add(call.width);
        Ok(())
    }

    fn cursor(&mut self, call: &mut mac_abi::quickdraw::CursorCall) -> Result<(), Status>
    {
        Toolbox::cursor(self, call, &[])
    }

    fn ticks(&self) -> u32
    {
        Self::when()
    }
}

impl super::quickdraw::worlds::WorldHost for Toolbox
{
    fn offscreen_worlds(&mut self) -> &mut super::quickdraw::worlds::Worlds
    {
        &mut self.worlds
    }
}

impl super::windows::lifetime::WindowHost for Toolbox
{
    fn release_window(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                      -> Result<(), super::handles::cleanup::Failure>
    {
        let owner = ticket.key().owner().incarnation();
        let id = self.windows.releasing_id(&ticket);
        if let Some(id) = id
           && self.drag.prepare_window_close(id.0).is_err()
        {
            return Err(super::handles::cleanup::Failure::drag_effect_storage(ticket,
                                                                           self.drag.pending_effects()));
        }
        // User DISPOSE is asynchronous. Accepted custody belongs to the SAME
        // WM debt/job; only its later affine close receipt may remove UI children
        // or release a dialog parent. Do not run that suffix at ticket adoption.
        if self.windows.releasing_uses_process_definition(&ticket) == Some(true)
        {
            return self.windows.release(ticket).map(|_| ());
        }
        let children = match id
        {
            Some(id) =>
            {
                match self.controls.prepare_window_removal(id.0)
                {
                    Ok(children) => Some(children),
                    Err(_) =>
                    {
                        return Err(super::handles::cleanup::Failure::window_controls_pending(ticket));
                    }
                }
            }
            None => None
        };
        let canonical = self.windows.releasing_has_trusted_close(&ticket);
        let result = self.windows.release(ticket);
        if canonical
        {
            drop(children);
            return result.map(|_| ());
        }
        if let Some(children) = children
        {
            children.commit();
        }
        if let Some(id) = id
        {
            // A window can be walked before its Alert parent. End that protocol
            // without foregrounding its owner or imposing a kind order.
            let alert = self.alerts_for_owner(owner).and_then(|manager| manager.take_window(id.0));
            drop(alert);
            let before = self.drag.feedback();
            let drag_cleanup = self.drag.forget_window(id.0, Self::when());
            self.drag_effects(before);
            if let Err(status) = drag_cleanup
            {
                serial!("DRAG_WINDOW_CLEANUP_DEFERRED window={} status={status}", id.0);
            }
            self.scroll_boxes.remove_window(id.0);
            self.presentation.content.retain(|content| content.id != id);
            self.presentation.content_damage.retain(|(target, _)| *target != id);
            self.presentation.ordinary_underlay.forget(SceneTarget::from(id));
            self.presentation.pending_content.retain(|content| content.id != id);
            self.forget_cursor_window(id);
            self.presentation.redraw = true;
        }
        result.map(|_| ())
    }

    fn release_window_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                              -> Result<(), super::handles::cleanup::Failure>
    {
        self.windows.release_storage(ticket)
    }
}

/// Damage accounting must remain available after ordinary allocation refuses.
/// A full copy is the last resort for exhausted bookkeeping; the compositor's
/// successful backbuffer and the normal cursor/feedback overlays stay in use.
fn accumulate_presentation_damage(damage: &mut Vec<Rect>, copy_full: &mut bool, rect: Rect)
{
    if rect.is_empty() || *copy_full
    {
        return;
    }
    if damage.len() == damage.capacity()
       && shared_scene_allocation(|| damage.try_reserve(1)).is_err()
    {
        *copy_full = true;
        return;
    }
    damage.push(rect);
}

/// Retiring a control must not make allocation failure fatal. Preserve exact
/// old bounds normally; if their metadata cannot grow, coalesce only this
/// object's removed bounds and keep its cleanup independent of heap headroom.
fn accumulate_removed_damage(damage: &mut Vec<Rect>, coalesced: &mut Option<Rect>, rect: Rect)
{
    if rect.is_empty()
    {
        return;
    }
    if let Some(previous) = coalesced.as_mut()
    {
        *previous = super::quickdraw::geometry::union_rect(*previous, rect);
        return;
    }
    if damage.len() == damage.capacity()
       && shared_scene_allocation(|| damage.try_reserve(1)).is_err()
    {
        let bounds = damage.iter().copied().fold(rect, super::quickdraw::geometry::union_rect);
        damage.clear();
        *coalesced = Some(bounds);
        return;
    }
    damage.push(rect);
}

/// Shared scene/damage indices retain ordinary System backing. The service
/// consumes its boot-admitted headroom; refused growth stays fallible.
fn shared_scene_allocation<R>(operation: impl FnOnce() -> R) -> R
{
    crate::context::without_recovery(|| InvocationGate::with_system_allocation(operation))
}
