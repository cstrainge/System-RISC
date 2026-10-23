//! System-owned event routing, retained application drawing and compositing.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, string::String, vec::Vec};

use mac_abi::*;

use super::quickdraw::{SurfaceExt,
                       fonts::FontManager,
                       fonts_service,
                       retained::{Content, DesktopLayer, RenderCommand},
                       service};
use crate::{loader::Definitions,
            platform,
            toolbox::{events::{BTN_STATE, EventManager,
                               input::{Decoder, Input}},
                      quickdraw::{Canvas, WHITE, font::GENEVA_9},
                      windows::{Action, WindowId, WindowManager, WindowPart}},
            virtio_input};

#[path = "crashes/runtime.rs"]
mod crash_runtime;
#[path = "standard_file/runtime.rs"]
mod standard_file_runtime;
#[path = "dialogs/runtime.rs"]
mod dialog_runtime;
#[path = "events/runtime.rs"]
mod event_runtime;
#[path = "drag/runtime.rs"]
mod drag_runtime;
#[path = "textedit/runtime.rs"]
mod textedit_runtime;
#[path = "processes/runtime.rs"]
mod local;
const MAX_RETAINED_BYTES: usize = 2 * 1024 * 1024;
pub struct Desktop
{
    drag:                             super::drag::Manager,
    standard_file:                    super::standard_file::Manager,
    scroll_boxes:                     super::controls::scrollbox::Manager,
    controls:                         super::controls::objects::Manager,
    crash:                            super::crashes::Manager,
    dialogs:                          super::dialogs::DialogState,
    desktop:                          DesktopLayer,
    menus:                            super::menus::Manager,
    cursors:                          super::quickdraw::cursors::Cursors,
    busy_cursor:                      super::quickdraw::cursors::Cursors,
    responsiveness:                   super::quickdraw::responsiveness::Responsiveness,
    text_cursor:                      mac_abi::quickdraw::MonoCursor,
    worlds:                           super::quickdraw::worlds::Worlds,
    classic_font:                     mac_abi::quickdraw::FontSelection,
    legacy:                           crate::context::LegacyGlobals,
    pub windows:                      WindowManager,
    first_window:                     WindowId,
    events:                           Box<EventManager>,
    textedit:                         super::textedit::Manager,
    definitions:                      Definitions,
    content:                          Vec<Content>,
    pending_content:                  Vec<Content>,
    content_cache:                    super::quickdraw::content_cache::ContentCache,
    menu_cache:                       super::quickdraw::menu_cache::MenuBarCache,
    decoders:                         [Decoder; 2],
    pub revision:                     u32,
    pub updates:                      u32,
    last_key:                         u8,
    last_action:                      &'static str,
    redraw:                           bool,
    content_damage:                   Vec<(WindowId, super::quickdraw::region::Region)>,
    outline_damage:                   Vec<Rect>,
    outline_under:                    Box<super::quickdraw::outline_overlay::OutlineUnder>,
    drag_highlight_under:             Box<super::quickdraw::outline_overlay::OutlineUnder>,
    cursor_dirty:                     bool,
    cursor_bounds:                    Rect,
    activity:                         u64,
    last_observed:                    (u64, u32, u64),
    presentations:                    u64,
    compositions:                     u64,
    cdef:                             DefinitionEntry,
    mdef:                             DefinitionEntry,
    owner:                            u64,
    foreground:                       u64,
    locals:                           Vec<local::Saved>,
    window_owners:                    Vec<(WindowId, u64)>,
    pub(in crate::toolbox) processes: super::processes::Manager,
    app_menu:                         super::processes::menu::ApplicationMenu
}

impl Desktop
{
    fn retained_bytes(&self) -> usize
    {
        self.desktop.storage()
        + self.content.iter().chain(&self.pending_content).map(Content::storage).sum::<usize>()
    }

    fn content_mut(&mut self, id: u32) -> Result<&mut Content, Status>
    {
        if id == mac_abi::quickdraw::DESKTOP_TARGET
        {
            if self.owner != crate::context::finder_app().0
            {
                return Err(ACCESS_DENIED);
            }
            self.desktop.pending_mut()
        }
        else
        {
            if !self.owns(WindowId(id))
            {
                return Err(if self.windows.window(WindowId(id)).is_some()
                           {
                               ACCESS_DENIED
                           }
                           else
                           {
                               NOT_FOUND
                           });
            }
            self.pending_content
                .iter_mut()
                .chain(&mut self.content)
                .find(|content| content.id.0 == id)
                .ok_or(NOT_FOUND)
        }
    }

    /// A retained update is private until EndUpdate. Other threads can poll,
    /// move the pointer and present the previously committed scene meanwhile.
    fn content_changed(&mut self, id: u32)
    {
        if let Some(window) = self.windows.window(WindowId(id))
        {
            self.content_changed_rect(id, window.local_bounds());
        }
    }

    fn content_changed_rect(&mut self, id: u32, rect: Rect)
    {
        if self.pending_content.iter().any(|content| content.id.0 == id)
        {
            return;
        }
        self.content_changed_region(id, super::quickdraw::region::Region::from_rect(rect));
    }

    fn content_changed_region(&mut self, id: u32, region: super::quickdraw::region::Region)
    {
        use super::quickdraw::region::{BooleanOp, Region};
        if self.pending_content.iter().any(|content| content.id.0 == id)
        {
            return;
        }
        let local = if id == mac_abi::quickdraw::DESKTOP_TARGET
        {
            if self.desktop.is_updating()
            {
                return;
            }
            self.desktop.bounds()
        }
        else
        {
            let Some(window) = self.windows.window(WindowId(id))
            else
            {
                return;
            };
            window.local_bounds()
        };
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
        };
        if region.is_empty()
        {
            return;
        }
        for band in region.bands()
        {
            for span in &band.spans
            {
                self.content_cache
                    .invalidate_rect(id, Rect::new(span.left, band.top, span.right, band.bottom));
            }
        }
        self.queue_content_damage(WindowId(id), region);
    }

    fn queue_content_damage(&mut self, id: WindowId, region: super::quickdraw::region::Region)
    {
        use super::quickdraw::region::BooleanOp;
        if region.is_empty()
        {
            return;
        }
        if let Some((_, previous)) =
            self.content_damage.iter_mut().find(|(window, _)| *window == id)
        {
            *previous = previous.combine(&region, BooleanOp::Union);
        }
        else
        {
            self.content_damage.push((id, region));
        }
    }

    fn apply_content_outline(&mut self, id: WindowId, next: Option<Rect>) -> Result<(), Status>
    {
        let (edges, origin) = if id.0 == mac_abi::quickdraw::DESKTOP_TARGET
        {
            (self.desktop.set_outline(next), Point::default())
        }
        else
        {
            (self.windows.content_outline(id, next)?,
             {
                 let bounds = self.windows.window(id).ok_or(NOT_FOUND)?.bounds;
                 Point { h: bounds.left,
                         v: bounds.top }
             })
        };
        if let Some(edges) = edges
        {
            self.outline_damage.extend(edges.into_iter()
                                            .filter(|rect| !rect.is_empty())
                                            .map(|mut rect| {
                                                rect.offset(origin.h, origin.v);
                                                rect
                                            }));
        }
        Ok(())
    }

    pub fn new(mut definitions: Definitions) -> Result<Self, Status>
    {
        definitions.resolve(*b"WDEF", 0).map_err(crate::loader::status)?;
        let cdef = definitions.resolve(*b"CDEF", 0).map_err(crate::loader::status)?;
        let mdef = definitions.resolve(*b"MDEF", 0).map_err(crate::loader::status)?;
        let devices = virtio_input::initialize();
        if devices != 2
        {
            serial!("INPUT_UNAVAILABLE devices={}", devices);
            return Err(NOT_SUPPORTED);
        }
        let text_cursor = super::quickdraw::cursors::decode_mono(
            &crate::kernel::resources().ok_or(NOT_SUPPORTED)?
                .get(mac_abi::resources::SYSTEM, ResourceKey { kind: *b"CURS", id: 1 })?.1.data)?;
        serial!("VIRTIO_INPUT=READY devices={}", devices);
        let mut busy_cursor = super::quickdraw::cursors::Cursors::default();
        busy_cursor.resource_request(&mut mac_abi::quickdraw::CursorResourceCall {
            operation: mac_abi::quickdraw::CURSOR_RESOURCE_ANIMATE,
            scope: mac_abi::resources::SYSTEM,
            id: -6078,
            interval: 6,
            ..Default::default()
        }, Point::default(), Self::when(), |kind, id, scope| {
            crate::kernel::resources()?.get(scope, ResourceKey { kind, id }).ok()
                .map(|(_, resource)| resource.data.to_vec())
        })?;
        serial!("UI_OBSERVER address={} bytes=128 version=1",
                super::quickdraw::observer::address());
        let mut processes = super::processes::Manager::new();
        let finder = processes.add("Finder", 0, 0, 0)?;
        processes.activate(finder)?;
        crate::context::set_finder_app(finder);
        Ok(Self { first_window: WindowId(0),
                  crash: Default::default(),
                  standard_file: Default::default(),
                  scroll_boxes: Default::default(),
                  controls: Default::default(),
                  dialogs: Default::default(),
                  menus: super::menus::Manager::new(mdef, crate::machine::traps::gateway),
                  desktop: DesktopLayer::new(Rect::new(0, 21, 640, 480)),
                  cursors: Default::default(),
                  busy_cursor,
                  responsiveness: Default::default(),
                  text_cursor,
                  worlds: Default::default(),
                  classic_font: Default::default(),
                  legacy:Default::default(),
                  windows: WindowManager::with_gate(crate::machine::traps::gateway),
                  events: Box::new(EventManager::new()),
                  drag: Default::default(),
                  textedit: Default::default(),
                  definitions,
                  content: Vec::new(),
                  pending_content: Vec::new(),
                  content_cache: Default::default(),
                  menu_cache: Default::default(),
                  decoders: [Decoder::new(), Decoder::new()],
                  revision: 0,
                  updates: 0,
                  last_key: 0,
                  last_action: "READY",
                  redraw: true,
                  content_damage: Vec::new(),
                  outline_damage:Vec::new(),
                  outline_under:Box::default(), drag_highlight_under:Box::default(),
                  cursor_dirty: true,
                  cursor_bounds: Rect::ZERO,
                  activity: 0,
                  last_observed: (u64::MAX, u32::MAX, u64::MAX),
                  presentations: 0,
                  compositions: 0,
                  cdef,mdef,owner:finder,foreground:finder,locals:alloc::vec![local::Saved::new(finder,mdef)],window_owners:Vec::new(),
                  processes,
                  app_menu:super::processes::menu::ApplicationMenu::new(mdef,crate::machine::traps::gateway,crate::kernel::resources().ok_or(NOT_SUPPORTED)?)? })
    }

    fn uptime() -> u64
    {
        let raw = platform::ticks();
        (raw / 10_000_000) * 60 + (raw % 10_000_000) * 60 / 10_000_000
    }

    fn when() -> u32
    {
        Self::uptime() as u32
    }

    pub fn menu(&mut self, call: &mut mac_abi::menus::MenuCall) -> Status
    {
        let Some(resources) = crate::kernel::resources()
        else
        {
            return NOT_SUPPORTED;
        };
        if call.handle == 0 && call.menu_id == super::processes::menu::MENU_ID
        {
            return self.app_menu.query(call).map_or_else(|e| e, |_| OK);
        }
        let result = self.menus.request(call, resources);
        if matches!(result, Ok(true)) && self.owner == self.foreground
        {
            self.menu_cache.invalidate();
            self.redraw = true;
        }
        result.map_or_else(|status| status, |_| OK)
    }

    fn menu_selection(&mut self, selection: mac_abi::menus::Selection, when: u32)
    {
        self.events.post_system(Event { payload: 0,
                                        kind: EventKind::MenuCommand,
                                        message: ((selection.menu_id as u16 as u32) << 16)
                                                 | u32::from(selection.item),
                                        when,
                                        where_: self.events.mouse,
                                        modifiers: self.events.modifiers });
        self.last_action = "MENU";
    }

    pub fn window(&mut self, operation: u32, request: &mut WindowRequest) -> Status
    {
        if operation != GET_WINDOW
        {
            crate::trace::record(crate::trace::WINDOW,
                                 self.owner,
                                 operation as u64,
                                 request.id as u64,
                                 0,
                                 0);
        }
        let when = Self::when();
        if request.id == 0 && operation == GET_WINDOW
        {
            let Some(id) = self.front_owned()
            else
            {
                return NOT_FOUND;
            };
            request.id = id.0;
        }
        if !matches!(operation, NEW_WINDOW | SELECT_DESKTOP)
           && request.id != 0
           && !self.owns(WindowId(request.id))
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
        let active_before = self.windows.active_window();
        let mut patch_damage = None;
        match operation
        {
            NEW_WINDOW =>
            {
                if request.title_length as usize > request.title.len() || request.procedure < 0
                {
                    return BAD_ARGUMENT;
                }
                let title = match core::str::from_utf8(&request.title
                                                           [..request.title_length as usize])
                {
                    Ok(title) => title,
                    Err(_) => return BAD_ARGUMENT
                };
                let entry = match self.definitions.resolve(*b"WDEF", request.procedure >> 4)
                {
                    Ok(entry) => entry,
                    Err(error) => return crate::loader::status(error)
                };
                let Some(id) = self.windows.new_window_with_activation(request.bounds,
                                                                       title,
                                                                       self.owner as u32,
                                                                       (request.minimum_width,
                                                                        request.minimum_height),
                                                                       request.procedure,
                                                                       entry,
                                                                       &mut self.events,
                                                                       when,
                                                                       self.owner
                                                                       == self.foreground)
                else
                {
                    return BAD_ARGUMENT;
                };
                if let Err(status) =
                    self.drag.ensure_receiver(self.owner,
                                              crate::context::current_thread_context().identity()
                                                                                      .thread
                                                                                      .0,
                                              id.0)
                {
                    self.windows.dispose_window(id, &mut self.events, when);
                    return status;
                }
                request.id = id.0;
                if self.first_window.0 == 0
                {
                    self.first_window = id;
                }
                self.window_owners.push((id, self.owner));
                if self.owner != self.foreground
                {
                    if self.processes.get(self.owner).is_ok_and(|p| p.info.hidden != 0)
                    {
                        self.windows.set_hidden(id, true);
                    }
                    let foreground: Vec<_> =
                        self.windows
                            .back_to_front()
                            .filter(|w| self.window_owner(w.id) == Some(self.foreground))
                            .map(|w| w.id)
                            .collect();
                    self.windows.restore_group(&foreground, &mut self.events, when);
                }
                self.content.push(Content::new(id));
                self.last_action = "NEW";
            }
            CLOSE_WINDOW =>
            {
                if !self.windows.dispose_window(id, &mut self.events, when)
                {
                    return NOT_FOUND;
                }
                let before = self.drag.feedback();
                self.drag.forget_window(id.0, when);
                self.drag_effects(before);
                self.scroll_boxes.remove_window(id.0);
                self.controls.remove_window(id.0);
                self.content.retain(|content| content.id != id);
                self.pending_content.retain(|content| content.id != id);
                self.forget_cursor_window(id);
                self.window_owners.retain(|(window, _)| *window != id);
                self.last_action = "CLOSE";
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
            SELECT_DESKTOP =>
            {
                if self.owner != crate::context::finder_app().0
                {
                    return ACCESS_DENIED;
                }
                if self.foreground != crate::context::finder_app().0
                   && let Err(status) =
                       self.activate_process_window(crate::context::finder_app().0, None)
                {
                    return status;
                }
                self.windows.deactivate_all(&mut self.events, when);
                self.last_action = "DESKTOP_SELECT";
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
            SET_STANDARD_BOUNDS =>
            {
                let bounds = match request.reserved
                {
                    0 => Some(request.bounds),
                    1 => None,
                    _ => return BAD_ARGUMENT
                };
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
                let Some(window) = self.windows.window(id)
                else
                {
                    return NOT_FOUND;
                };
                if window.updating
                {
                    return BAD_ARGUMENT;
                }
                if self.pending_content.try_reserve(1).is_err()
                {
                    return NO_MEMORY;
                }
                let Some(content) = self.content.iter().find(|content| content.id == id)
                else
                {
                    return NOT_FOUND;
                };
                if self.retained_bytes().saturating_add(content.port.storage()) > MAX_RETAINED_BYTES
                {
                    return NO_MEMORY;
                }
                let mut pending = Content::new(id);
                pending.port = content.port.clone();
                // First cut repaints each complete rectangular content port.
                let local = window.local_bounds();
                self.windows.invalidate(id, local);
                let Some(damage) = self.windows.begin_update(id)
                else
                {
                    return NOT_FOUND;
                };
                request.bounds = damage;
                self.pending_content.push(pending);
                self.textedit.begin_cursor_update(id);
                self.updates += 1;
            }
            END_UPDATE =>
            {
                let Some(index) = self.pending_content.iter().position(|content| content.id == id)
                else
                {
                    return BAD_ARGUMENT;
                };
                let used = self.retained_bytes();
                let bounds = self.windows.window(id).unwrap().local_bounds();
                let Some(content) = self.content.iter_mut().find(|content| content.id == id)
                else
                {
                    return NOT_FOUND;
                };
                let mut pending = self.pending_content.swap_remove(index);
                let outline = pending.outline.take();
                if pending.is_patch()
                {
                    match content.commit_patch(pending, bounds, used, MAX_RETAINED_BYTES)
                    {
                        Ok(mask) => patch_damage = Some(mask),
                        Err(status) =>
                        {
                            self.textedit.begin_cursor_update(id);
                            self.windows.end_update(id);
                            return status;
                        }
                    }
                }
                else
                {
                    *content = pending;
                }
                if let Some(mask) = &patch_damage
                {
                    self.textedit.commit_partial_cursor_update(id, mask);
                }
                else
                {
                    self.textedit.commit_cursor_update(id);
                }
                self.windows.end_update(id);
                if let Some(outline) = outline
                   && let Err(status) = self.apply_content_outline(id, outline)
                {
                    return status;
                }
            }
            _ => return NOT_SUPPORTED
        }
        self.route_events();
        self.window_owners.retain(|(id, _)| self.windows.window(*id).is_some());
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
        else if !matches!(operation, BEGIN_UPDATE | INVALIDATE_WINDOW | SET_CLOSE_DEFERRED)
                  && (operation != SELECT_WINDOW || self.windows.active_window() != active_before)
        {
            self.redraw = true;
        }
        // Retained drawing is committed before EndUpdate returns. Applications
        // can report readiness only after the corresponding pixels are visible.
        if operation == END_UPDATE
        {
            self.present();
        }
        OK
    }

    pub fn control_definition(&mut self, id: i16) -> Result<DefinitionEntry, Status>
    {
        self.definitions.resolve(*b"CDEF", id).map_err(crate::loader::status)
    }

    pub fn scroll_box(&mut self, call: &mut mac_abi::scrollbox::Call)
                      -> Result<Option<super::controls::scrollbox::Drawing>, Status>
    {
        use mac_abi::scrollbox::*;
        let window = if call.operation == CREATE
        {
            call.window
        }
        else
        {
            self.scroll_boxes.window(call.id, self.owner)?
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
        let entry = self.definitions.resolve(*b"CDEF", 1).map_err(crate::loader::status)?;
        let drawing = self.scroll_boxes.request(self.owner,
                                                 call,
                                                 crate::kernel::fonts().ok_or(NOT_SUPPORTED)?,
                                                 entry,
                                                 crate::machine::traps::gateway)?;
        if !call.surface.is_null()
        {
            return Ok(drawing);
        }
        if call.operation == DISPOSE && window != 0
        {
            let mut removed = Vec::new();
            for content in self.content
                               .iter_mut()
                               .chain(self.pending_content.iter_mut())
                               .filter(|content| content.id.0 == window)
            {
                content.retain_commands(|command| {
                    if let RenderCommand::ScrollBox(drawing) = command
                       && drawing.id == call.id
                    {
                        removed.extend(drawing.bounds());
                    }
                    !matches!(command, RenderCommand::ScrollBox(drawing) if drawing.id == call.id)
                });
            }
            for rect in removed
            {
                self.content_changed_rect(window, rect);
            }
        }

        if let Some(drawing) = drawing
        {
            let bounds = drawing.bounds();
            let mut old_bounds = None;
            let used = self.retained_bytes();
            let size = drawing.storage();
            let id = drawing.id;
            let content = self.content_mut(window)?;
            let previous = content.iter_commands_mut().find(|command| {
                matches!(command, RenderCommand::ScrollBox(old) if old.id == id)
            });
            if let Some(command) = previous
            {
                if let RenderCommand::ScrollBox(old) = command
                {
                    old_bounds = Some(old.bounds());
                }
                if used - command.storage() + size > MAX_RETAINED_BYTES
                {
                    return Err(NO_MEMORY);
                }
                *command = RenderCommand::ScrollBox(drawing);
            }
            else
            {
                if used + size > MAX_RETAINED_BYTES || content.commands.len() >= 512
                {
                    return Err(NO_MEMORY);
                }
                content.commands.try_reserve(1).map_err(|_| NO_MEMORY)?;
                content.commands.push(RenderCommand::ScrollBox(drawing));
            }
            for rect in old_bounds.into_iter().flatten().chain(bounds)
            {
                self.content_changed_rect(window, rect);
            }
            if !self.pending_content.iter().any(|content| content.id.0 == window)
            {
                self.present();
            }
        }
        Ok(None)
    }

    fn managed_control(&mut self, call: &mut mac_abi::controls::ControlCall) -> Result<(), Status>
    {
        use mac_abi::controls::*;
        let window = if matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            call.window
        }
        else
        {
            self.controls.window(call.id, self.owner)?
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
        let definition = self.controls.definition(call, self.owner)?;
        let entry = self.control_definition(definition)?;
        // A deactivated/hidden owner cannot continue a capture after app switching.
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && window != 0
           && (self.owner != self.foreground
               || self.windows.active_window() != Some(WindowId(window)))
        {
            call.operation = CANCEL_TRACKING;
        }
        let drawing =
            self.controls.request(self.owner, call, entry, crate::machine::traps::gateway)?;
        if call.operation == DISPOSE_OBJECT
        {
            let mut removed = Vec::new();
            for content in self.content
                               .iter_mut()
                               .chain(self.pending_content.iter_mut())
                               .filter(|c| c.id.0 == window)
            {
                content.retain_commands(|command| {
                           if let RenderCommand::Control(d) = command
                              && d.id == call.id
                           {
                               removed.push(d.bounds());
                               return false;
                           }
                           true
                       });
            }
            for bounds in removed
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
            let used = self.retained_bytes();
            let size = drawing.storage();
            let content = self.content_mut(window)?;
            let old = content.iter_commands_mut()
                             .find(|c| matches!(c, RenderCommand::Control(d) if d.id == call.id));
            let old_bounds = if let Some(command) = old
            {
                if used - command.storage() + size > MAX_RETAINED_BYTES
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
                if used + size > MAX_RETAINED_BYTES || content.commands.len() >= 512
                {
                    return Err(NO_MEMORY);
                }
                content.commands.try_reserve(1).map_err(|_| NO_MEMORY)?;
                content.commands.push(RenderCommand::Control(drawing));
                None
            };
            if let Some(old) = old_bounds
               && old != bounds
            {
                self.content_changed_rect(window, old);
            }
            self.content_changed_rect(window, bounds);
            // A control state change commits its own pixels before returning.
            // Full window updates remain private until EndUpdate.
            if !self.pending_content.iter().any(|c| c.id.0 == window)
            {
                self.present();
            }
        }
        Ok(())
    }

    pub fn control(&mut self, call: &mut mac_abi::controls::ControlCall) -> Status
    {
        if call.operation >= mac_abi::controls::CREATE_STANDARD
        {
            return self.managed_control(call).map_or_else(|e| e, |_| OK);
        }
        if !call.surface.is_null()
        {
            return BAD_ARGUMENT;
        }
        if call.window != 0 && self.windows.window(WindowId(call.window)).is_none()
        {
            return NOT_FOUND;
        }
        let id = match super::controls::definition_id(call)
        {
            Ok(id) => id,
            Err(e) => return e
        };
        let entry = match self.definitions.resolve(*b"CDEF", id)
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
        if self.retained_bytes() + drawing.storage() > MAX_RETAINED_BYTES
        {
            return NO_MEMORY;
        }
        let content = match self.content_mut(call.window)
        {
            Ok(c) => c,
            Err(e) => return e
        };
        let bounds = drawing.bounds();
        content.commands.push(RenderCommand::Control(drawing));
        self.content_changed_rect(call.window, bounds);
        OK
    }

    pub fn draw(&mut self, id: u32, command: DrawCommand) -> Status
    {
        if self.retained_bytes() + core::mem::size_of::<DrawCommand>() > MAX_RETAINED_BYTES
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
        content.commands.push(RenderCommand::Legacy(alloc::boxed::Box::new(command)));
        self.content_changed(id);
        OK
    }

    fn masked_update(&mut self, call: &mac_abi::quickdraw::GraphicsCall, bytes: &[u8])
                     -> Result<(), Status>
    {
        use mac_abi::quickdraw::*;

        use super::quickdraw::region::{BooleanOp, Region};
        let id = WindowId(call.window);
        // Use the ordinary owner/generation gate, including stale window IDs.
        let desktop = call.window == DESKTOP_TARGET;
        if desktop
        {
            if self.owner != crate::context::finder_app().0
            {
                return Err(ACCESS_DENIED);
            }
        }
        else
        {
            self.content_mut(call.window)?;
        }
        if matches!(call.operation, QD_WINDOW_BEGIN_MASK | QD_WINDOW_BEGIN_RECTS)
        {
            let bounds = if desktop
            {
                if self.desktop.is_updating()
                {
                    return Err(BAD_ARGUMENT);
                }
                self.desktop.bounds()
            }
            else
            {
                let window = self.windows.window(id).ok_or(NOT_FOUND)?;
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
                }))
            }
            else
            {
                Region::from_be_bytes(bytes).map_err(|_| BAD_ARGUMENT)?
            };
            let mask = mask.combine(&Region::from_rect(bounds), BooleanOp::Intersection);
            if desktop
            {
                return self.desktop.begin_mask(mask, self.retained_bytes(), MAX_RETAINED_BYTES);
            }
            if mask.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            self.pending_content.try_reserve(1).map_err(|_| NO_MEMORY)?;
            let content = self.content.iter().find(|c| c.id == id).ok_or(NOT_FOUND)?;
            let pending = Content::pending_patch(id, content.port.clone(), mask);
            if self.retained_bytes().saturating_add(pending.storage()) > MAX_RETAINED_BYTES
            {
                return Err(NO_MEMORY);
            }
            if !self.windows.begin_partial_update(id)
            {
                return Err(BAD_ARGUMENT);
            }
            self.pending_content.push(pending);
            self.textedit.begin_cursor_update(id);
        }
        else
        {
            if desktop
            {
                if call.operation == QD_WINDOW_CANCEL_MASK
                {
                    return self.desktop.cancel_mask();
                }
                let outline = self.desktop.pending_outline();
                let mask = self.desktop.commit_mask(self.retained_bytes(), MAX_RETAINED_BYTES)?;
                if let Some(outline) = outline
                {
                    self.apply_content_outline(id, outline)?;
                }
                self.content_changed_region(id.0, mask);
                self.present();
                return Ok(());
            }
            let index = self.pending_content
                            .iter()
                            .position(|c| c.id == id && c.is_patch())
                            .ok_or(BAD_ARGUMENT)?;
            if call.operation == QD_WINDOW_CANCEL_MASK
            {
                self.pending_content.swap_remove(index);
                self.textedit.begin_cursor_update(id);
                self.windows.end_update(id);
            }
            else
            {
                let mut request = mac_abi::WindowRequest { id:             id.0,
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
        use mac_abi::quickdraw::{DESKTOP_TARGET, QD_DESKTOP_BEGIN, QD_DESKTOP_BOUNDS,
                                 QD_DESKTOP_CANCEL, QD_DESKTOP_END, QD_GET_STATE};
        if call.operation == mac_abi::quickdraw::QD_WINDOW_OUTLINE
        {
            if call.verb > 1
            {
                return Err(BAD_ARGUMENT);
            }
            let desktop = call.window == DESKTOP_TARGET;
            if desktop
            {
                if self.owner != crate::context::finder_app().0
                {
                    return Err(ACCESS_DENIED);
                }
            }
            else
            {
                self.content_mut(call.window)?;
            }
            let next = (call.verb == 1 && !call.bounds.is_empty()).then_some(call.bounds);
            let id = WindowId(call.window);
            if desktop && self.desktop.is_updating()
            {
                self.desktop.pending_mut()?.outline = Some(next);
            }
            else if let Some(pending) =
                self.pending_content.iter_mut().find(|content| content.id == id)
            {
                pending.outline = Some(next);
            }
            else
            {
                self.apply_content_outline(id, next)?;
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
        if matches!(call.operation,
                    QD_DESKTOP_BEGIN | QD_DESKTOP_END | QD_DESKTOP_CANCEL | QD_DESKTOP_BOUNDS)
        {
            if self.owner != crate::context::finder_app().0 && call.operation != QD_DESKTOP_BOUNDS
            {
                return Err(ACCESS_DENIED);
            }
            let outline = self.desktop.pending_outline();
            let committed = self.desktop.request(call)?;
            if committed
            {
                if let Some(outline) = outline
                {
                    self.apply_content_outline(WindowId(DESKTOP_TARGET), outline)?;
                }
                self.content_cache.invalidate(DESKTOP_TARGET);
                self.last_action = "DESKTOP";
                self.redraw = true;
                self.present();
            }
            return Ok(());
        }
        if call.window == DESKTOP_TARGET && call.operation == QD_GET_STATE
        {
            call.state = self.desktop.port().wire();
            return Ok(());
        }
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            return self.worlds.get_mut(call.window)?.graphics(call, points, region);
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        let saved = content.port.clone();
        if let Some(drawing) = service::graphics(&mut content.port, call, points, region)?
        {
            if used + drawing.storage() > MAX_RETAINED_BYTES
            {
                content.port = saved;
                return Err(NO_MEMORY);
            }
            content.commands.push(RenderCommand::Graphics(drawing));
            self.content_changed(call.window);
        }
        Ok(())
    }

    pub fn copy_bits(&mut self, call: &mac_abi::quickdraw::BitmapCall, data: &[u8],
                     palette: &[u32], mask: &[u8])
                     -> Result<(), Status>
    {
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            let world = self.worlds.get_mut(call.window)?;
            let drawing = super::quickdraw::bitmaps_service::BitmapDrawing::new(call,
                                                                                data,
                                                                                palette,
                                                                                mask,
                                                                                world.port
                                                                                     .clone())?;
            return drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height));
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        let drawing = super::quickdraw::bitmaps_service::BitmapDrawing::new(call,
                                                                            data,
                                                                            palette,
                                                                            mask,
                                                                            content.port.clone())?;
        if used + drawing.storage() + drawing.port.storage() > MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        content.commands.push(RenderCommand::Bitmap(drawing));
        self.content_changed(call.window);
        Ok(())
    }

    pub fn cursor(&mut self, call: &mut mac_abi::quickdraw::CursorCall, colors: &[u32])
                  -> Result<(), Status>
    {
        let changed = self.cursors.request(call, colors, self.events.mouse)?;
        self.cursor_dirty |= changed && self.owner == self.foreground;
        Ok(())
    }

    pub fn cursor_resource(&mut self, call: &mut mac_abi::quickdraw::CursorResourceCall)
                           -> Result<(), Status>
    {
        let resources = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
        let changed =
            self.cursors.resource_request(call,
                                           self.events.mouse,
                                           Self::when(),
                                           |kind, id, scope| {
                                               resources.get(scope,
                                                             ResourceKey { kind,
                                                                           id })
                                                        .ok()
                                                        .map(|(_, resource)| resource.data.clone())
                                           })?;
        self.cursor_dirty |= changed && self.owner == self.foreground;
        Ok(())
    }

    /// The foreground application owns cursor policy; only an exposed editor
    /// content region can request an automatic I-beam. WDEF hit-testing excludes
    /// chrome and covered windows, while TextEdit accounts for its port clip.
    fn text_cursor_at_mouse(&self) -> bool
    {
        if !self.cursors.is_automatic()
           || self.menus.is_tracking()
           || self.app_menu.is_tracking()
           || self.windows.is_tracking()
        {
            return false;
        }
        let hit = self.windows.find_window(self.events.mouse);
        if hit.part != WindowPart::Content
        {
            return false;
        }
        hit.window
           .filter(|id| self.owns(*id))
           .and_then(|id| self.windows.window(id).map(|window| (id, window)))
           .is_some_and(|(id, window)| {
               let local = window.global_to_local(self.events.mouse);
               self.textedit.cursor_hit(id, local)
               || self.standard_file
                      .active(self.owner)
                      .and_then(|key| self.standard_file.get(key).ok())
                      .is_some_and(|d| {
                          d.window == id
                          && d.mode == mac_abi::standard_file::PUT
                          && d.sheet == super::standard_file::Sheet::Main
                          && d.editor.info().view.contains(local)
                      })
           })
    }

    pub fn world(&mut self, call: &mut mac_abi::quickdraw::WorldCall, pixels: &mut [u32])
                 -> Result<(), Status>
    {
        use mac_abi::quickdraw::*;
        match call.operation
        {
            WORLD_NEW =>
            {
                call.world = self.worlds.create(call.dimensions.h, call.dimensions.v)?;
            }
            WORLD_DISPOSE =>
            {
                self.worlds.dispose(call.world)?;
                return Ok(());
            }
            WORLD_INFO =>
            {}
            WORLD_READ | WORLD_WRITE =>
            {
                let world = self.worlds.get_mut(call.world)?;
                let count = world.pixels.len();
                call.pixel_length = count;
                if pixels.len() < count
                {
                    return Err(NO_MEMORY);
                }
                if call.operation == WORLD_READ
                {
                    pixels[..count].copy_from_slice(&world.pixels);
                }
                else
                {
                    for (out, input) in world.pixels.iter_mut().zip(pixels.iter())
                    {
                        *out = *input & 0xffffff;
                    }
                }
            }
            WORLD_COPY =>
            {
                let world = self.worlds.get(call.world)?;
                let data: Vec<u8> = world.pixels.iter().flat_map(|p| p.to_ne_bytes()).collect();
                let bitmap = BitmapCall { window: call.target,
                                          format: 32,
                                          row_bytes: world.width * 4,
                                          bounds: Rect::new(0, 0, world.width as i16,
                                                            world.height as i16),
                                          source_rect: call.source,
                                          destination_rect: call.destination,
                                          mode: call.mode,
                                          foreground: 0,
                                          background: 0xffffff,
                                          ..Default::default() };
                self.copy_bits(&bitmap, &data, &[], &[])?;
                return Ok(());
            }
            _ => return Err(NOT_SUPPORTED)
        }
        let world = self.worlds.get(call.world)?;
        call.dimensions = Point { h: world.width as i16,
                                  v: world.height as i16 };
        call.pixel_length = world.pixels.len();
        Ok(())
    }

    pub fn font_draw(&mut self, call: &mut mac_abi::quickdraw::FontCall, text: &str,
                     fonts: &FontManager)
                     -> Result<(), Status>
    {
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            return self.worlds.get_mut(call.window)?.text(call, text, fonts);
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        if content.commands.len() >= 512
        {
            return Err(NO_MEMORY);
        }
        if used + text.len() + content.port.storage() + 32 > MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        call.width = fonts.text_width(fonts_service::selection(call.selection), text)?;
        content.commands.push(RenderCommand::Text { font:     call.selection,
                                                    baseline: call.baseline,
                                                    text:     String::from(text),
                                                    color:    call.color,
                                                    port:     content.port.clone(),
                                                    vertical_ink: fonts.vertical_ink_bounds(fonts_service::selection(call.selection)).ok().flatten() });
        self.content_changed(call.window);
        Ok(())
    }

    pub fn picture(&mut self, call: &mac_abi::quickdraw::PictureCall,
                   mut drawing: super::quickdraw::pictures_service::Drawing, fonts: &FontManager)
                   -> Result<(), Status>
    {
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            let world = self.worlds.get_mut(call.window)?;
            drawing.port = world.port.clone();
            return drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height),
                                 fonts);
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        content.commands.push(RenderCommand::Picture(drawing));
        self.content_changed(call.window);
        Ok(())
    }

    pub fn image(&mut self, call: &mac_abi::images::ImageCall,
                 mut drawing: super::quickdraw::png::Drawing)
                 -> Result<(), Status>
    {
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            let world = self.worlds.get_mut(call.window)?;
            drawing.port = world.port.clone();
            drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height));
            return Ok(());
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        content.commands.try_reserve(1).map_err(|_| NO_MEMORY)?;
        content.commands.push(RenderCommand::Image(drawing));
        self.content_changed(call.window);
        Ok(())
    }

    pub fn icon(&mut self, call: &mac_abi::quickdraw::IconCall,
                mut drawing: super::quickdraw::icons_service::Drawing)
                -> Result<(), Status>
    {
        if super::quickdraw::worlds::Worlds::is_world(call.window)
        {
            let world = self.worlds.get_mut(call.window)?;
            drawing.port = world.port.clone();
            return drawing.paint(&mut Canvas::new(&mut world.pixels, world.width, world.height));
        }
        let used = self.retained_bytes();
        let content = self.content_mut(call.window)?;
        drawing.port = content.port.clone();
        if content.commands.len() >= 512 || used + drawing.storage() > MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        content.commands.push(RenderCommand::Icon(drawing));
        self.content_changed(call.window);
        Ok(())
    }

    pub fn definition(&mut self, call: &mut DefinitionCall) -> i64
    {
        if !call.header.supports::<DefinitionCall>()
        {
            return BAD_ABI as i64;
        }
        match self.definitions.resolve(call.kind.to_be_bytes(), call.id)
        {
            Ok(entry) => super::resources::definitions::invoke(entry, call),
            Err(error) =>
            {
                serial!("DEFINITION_UNAVAILABLE: {}", error);
                crate::loader::status(error) as i64
            }
        }
    }

    fn action(&mut self, action: Action, when: u32)
    {
        if let Action::Drag(id) | Action::Resize(id) = action
        {
            self.events.post_system(Event { kind: EventKind::OsEvent,
                                            message: mac_abi::events::WINDOW_GEOMETRY_CHANGED,
                                            payload: u64::from(id.0),
                                            ..self.events.null_event(when) });
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
                                                message: WINDOW_CLOSE_REQUEST | id.0,
                                                when,
                                                ..self.events.null_event(when) });
                "CLOSE_REQUEST"
            }
            Action::Close(id) =>
            {
                self.scroll_boxes.remove_window(id.0);
                self.controls.remove_window(id.0);
                self.content.retain(|content| content.id != id);
                self.pending_content.retain(|content| content.id != id);
                self.forget_cursor_window(id);
                self.events.post_system(Event { kind: EventKind::OsEvent,
                                                message: 0x02000000 | id.0,
                                                when,
                                                ..self.events.null_event(when) });
                "CLOSE"
            }
        };
    }

    fn input(&mut self, input: Input, when: u32)
    {
        let (kind, a, b, c) = match input
        {
            Input::Move(point) => (1, point.h as u64, point.v as u64, 0),
            Input::Button { point,
                            down } => (2, point.h as u64, point.v as u64, down as u64),
            Input::Key { mac_code,
                         character,
                         down,
                         repeat,
                         modifiers } =>
            {
                (3,
                 ((mac_code as u64) << 8) | character as u64,
                 down as u64 | ((repeat as u64) << 1),
                 modifiers as u64)
            }
        };
        crate::trace::record(crate::trace::INPUT, self.owner, kind, a, b, c);
        self.events.observe_input(input, when);
        if let Input::Move(point)
        | Input::Button { point,
                               .. } = input
        {
            // Clear obscuring at input time, even if several queued moves
            // bring the pointer back before the next framebuffer is painted.
            self.observe_cursor_motion(point);
        }
        self.activity = self.activity.wrapping_add(1);
        // Key delivery does not change window pixels. Repainting here stalls
        // dispatch on an obsolete frame before the application handles the key.
        // Pointer movement damages the cursor, not the retained scene. Tracking
        // overlays or a changed stacking plane have their own scene damage.
        if !matches!(input, Input::Key { .. })
        {
            self.cursor_dirty = true;
            self.redraw |= self.windows.is_tracking()
                           || self.menus.is_tracking()
                           || self.app_menu.is_tracking();
        }
        if self.crash_input(input, when)
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
        if self.drag_input(input, when)
        {
            return;
        }
        match input
        {
            Input::Move(point) =>
            {
                self.events.mouse = point;
                if self.app_menu.is_tracking()
                {
                    self.app_menu.hover(point);
                    return;
                }
                if self.menus.is_tracking()
                {
                    self.menus.hover(point);
                    return;
                }
                if let Some(action) = self.windows.track(point, false, &mut self.events, when)
                {
                    self.action(action, when);
                }
            }
            Input::Button { point,
                            down } =>
            {
                self.events.mouse = point;
                if down
                {
                    self.events.modifiers &= !BTN_STATE;
                }
                else
                {
                    self.events.modifiers |= BTN_STATE;
                }
                if down
                {
                    if self.app_menu.begin(point)
                    {
                        self.redraw = true;
                        return;
                    }
                    if self.menus.begin(point)
                    {
                        self.redraw = true;
                        return;
                    }
                    let hit = self.windows.find_window(point);
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
                        self.redraw |= !active
                                       || matches!(hit.part,
                                                   WindowPart::Drag
                                                   | WindowPart::Grow
                                                   | WindowPart::GoAway
                                                   | WindowPart::ZoomIn
                                                   | WindowPart::ZoomOut);
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
                            self.windows
                                .start_tracking_with_modifiers(hit, point, self.events.modifiers);
                        }
                        else if hit.part == WindowPart::Content && active
                        {
                            let local = self.windows.window(id).unwrap().global_to_local(point);
                            let button = self.content.iter().find(|c| c.id == id).and_then(|c| c.iter_commands().filter_map(|command| match command { RenderCommand::Legacy(c)=>Some(c),_=>None }).find(|command| command.kind == DRAW_BUTTON && command.bounds.contains(local)));
                            if let Some(command) = button
                            {
                                let mut state = control(command);
                                let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(), kind: u32::from_be_bytes(*b"CDEF"), id: 0, variation: 0, message: 1, reserved: 0, param: (((local.v as u16 as u32) << 16) | local.h as u16 as u32) as i64, object: (&mut state as *mut crate::toolbox::controls::ControlDefinitionState).cast(), gate: crate::machine::traps::gateway, surface: core::ptr::null_mut() };
                                if super::resources::definitions::invoke(self.cdef, &mut call) > 0
                                {
                                    self.events.post_system(Event { payload: 0,
                                                                    kind: EventKind::OsEvent,
                                                                    message: 0x01000000 | id.0,
                                                                    when,
                                                                    where_: point,
                                                                    modifiers: self.events
                                                                                   .modifiers });
                                }
                            }
                            self.events.post_system(Event { payload: 0,
                                                            kind: EventKind::MouseDown,
                                                            message: id.0,
                                                            when,
                                                            where_: point,
                                                            modifiers: self.events.modifiers });
                            self.last_action = "CONTENT";
                        }
                        else
                        {
                            self.last_action = "SELECT";
                        }
                    }
                    else
                    {
                        self.events.post_system(Event { payload: 0,
                                                        kind: EventKind::MouseDown,
                                                        message: 0,
                                                        when,
                                                        where_: point,
                                                        modifiers: self.events.modifiers });
                    }
                }
                else
                {
                    if self.app_menu.is_tracking()
                    {
                        if let Some(action) = self.app_menu.release(point)
                        {
                            self.application_action(action);
                        }
                        return;
                    }
                    if self.menus.is_tracking()
                    {
                        if let Some(selection) = self.menus.release(point)
                        {
                            self.menu_selection(selection, when);
                        }
                        return;
                    }
                    if let Some(action) = self.windows.track(point, true, &mut self.events, when)
                    {
                        self.action(action, when);
                    }
                    self.events.post_system(Event { payload: 0,
                                                    kind: EventKind::MouseUp,
                                                    message: 0,
                                                    when,
                                                    where_: point,
                                                    modifiers: self.events.modifiers });
                }
            }
            Input::Key { mac_code,
                         character,
                         down,
                         repeat,
                         modifiers } =>
            {
                self.events.modifiers = modifiers | (self.events.modifiers & BTN_STATE);
                if down
                   && modifiers & CMD_KEY != 0
                   && self.menus.has_key(character)
                   && let Some(selection) = self.menus.key(character)
                {
                    self.menu_selection(selection, when);
                    return;
                }
                // Menu policy can still reflect the previous frame while the
                // application opens or closes a window. Disabled shortcuts stay
                // available as keyboard input; only a real selection consumes it.
                if mac_code != 0xff
                {
                    if down
                    {
                        self.last_key = character;
                    }
                    self.events.post_system(Event { payload: 0,
                                                    kind: if !down
                                                    {
                                                        EventKind::KeyUp
                                                    }
                                                    else if repeat
                                                    {
                                                        EventKind::AutoKey
                                                    }
                                                    else
                                                    {
                                                        EventKind::KeyDown
                                                    },
                                                    message: (mac_code as u32) << 8
                                                             | character as u32,
                                                    when,
                                                    where_: self.events.mouse,
                                                    modifiers: self.events.modifiers });
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
        // Idle polling must not copy every application's menus and event queues
        // into and out of the foreground context. Decoder state changes that
        // affect applications (including modifiers) always produce an Input.
        if count == 0
        {
            return false;
        }
        let when = Self::when();
        let caller = self.owner;
        self.enter(self.foreground);
        for input in pending[..count].iter().flatten()
        {
            self.input(*input, when);
            self.route_events();
        }
        self.sync_input_state();
        self.enter(caller);
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
        self.cursor_dirty |= self.responsiveness.foreground(self.foreground, Self::when());
        self.cursor_dirty |= self.responsiveness.polled(self.owner, Self::when());
        self.poll_inputs();
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
        let available = event.is_some();
        let (event, mut position) = event.unwrap_or_else(|| {
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
        crate::trace::record(crate::trace::PRESENT, self.owner, 0, 0, 0, 0);
        self.tick_cursor();
        if !self.redraw
           && !self.cursor_dirty
           && self.content_damage.is_empty()
           && self.outline_damage.is_empty()
        {
            self.publish_observation();
            return;
        }
        let caller = self.owner;
        self.enter(self.foreground);
        // A tracked popup/outline is above content. Its backing scene must be
        // recomposed if an application changes pixels beneath that overlay.
        self.redraw |= !self.content_damage.is_empty()
                       && (self.menus.is_tracking()
                           || self.app_menu.is_tracking()
                           || self.windows.is_tracking());
        let full = self.redraw;
        if full || !self.content_damage.is_empty()
        {
            let cache_budget =
                super::quickdraw::content_cache::PIXEL_BUDGET.saturating_sub(self.scroll_boxes
                                                                                 .raster_storage())
                    .saturating_sub(super::quickdraw::menu_cache::BYTES);
            self.content_cache
                .preserve_headroom(crate::kernel::heap_available(), 2 * 1024 * 1024, cache_budget);
            self.revision = self.revision.wrapping_add(1);
            self.compositions = self.compositions.wrapping_add(1);
        }
        else if !self.outline_damage.is_empty()
        {
            self.revision = self.revision.wrapping_add(1);
        }
        let mut damaged = core::mem::take(&mut self.outline_damage);
        if self.redraw
        {
            let started = crate::trace::clock();
            let updating = self.windows.back_to_front().filter(|w| w.updating).count();
            let back = platform::back_buffer();
            let mut canvas = Canvas::new(back, 640, 480);
            background(&mut canvas);
            let cdef = self.cdef;
            let fonts = crate::kernel::fonts();
            let desktop_visible = self.processes
                                      .get(crate::context::finder_app().0)
                                      .is_ok_and(|p| p.info.hidden == 0);
            self.content_cache.begin_frame();
            self.content_cache.retain(|id| {
                                  if id == mac_abi::quickdraw::DESKTOP_TARGET
                                  {
                                      desktop_visible
                                  }
                                  else
                                  {
                                      self.windows.window(WindowId(id)).is_some()
                                      && !self.content
                                              .iter()
                                              .find(|content| content.id.0 == id)
                                              .is_some_and(|content| {
                                                  content.iter_commands().any(|command| {
                                                  matches!(command, RenderCommand::ScrollBox(_))
                                              })
                                              })
                                  }
                              });
            if desktop_visible
            {
                self.content_cache.paint(mac_abi::quickdraw::DESKTOP_TARGET,
                                         self.desktop.bounds(),
                                         &mut canvas,
                                         |canvas| {
                                             crate::kernel::with_resource_owner(crate::context::finder_app().0, || {
                                                 self.desktop.paint(canvas,
                                                                    fonts,
                                                                    |canvas, command| {
                                                                        paint(canvas, command, cdef)
                                                                    });
                                             });
                                         });
            }
            let content = &self.content;
            let cache = &mut self.content_cache;
            let cdef = self.cdef;
            platform::with_tracking_mask(|scratch| {
                self.windows.draw_with_scratch(&mut canvas, scratch, |window, canvas| {
                            let content = content.iter().find(|content| content.id == window.id);
                            let replay = |canvas: &mut Canvas<'_>| {
                                canvas.paint_rect(window.local_bounds(), WHITE);
                                if let Some(content) = content
                                {
                                    crate::kernel::with_resource_owner(u64::from(window.refcon), || {
                                        content.paint(canvas, fonts, |canvas, command| {
                                            paint(canvas, command, cdef)
                                        });
                                    });
                                }
                            };
                            // Managed scroll boxes already retain their viewport rasters.
                            // A second whole-window raster wastes heap and copies every frame.
                            let has_viewports = content.is_some_and(|content| {
                                content.iter_commands().any(|command| {
                                    matches!(command, RenderCommand::ScrollBox(_))
                                })
                            });
                            if has_viewports
                            {
                                replay(canvas);
                            }
                            else
                            {
                                cache.paint(window.id.0, window.bounds, canvas, replay);
                            }

                        })
            });
            self.menu_cache.paint(&mut canvas,
                                  (self.foreground,
                                   self.menus.selected_menu(),
                                   self.app_menu.is_tracking()),
                                  |canvas| {
                                      self.menus.draw_bar(canvas);
                                      self.app_menu.draw_bar(canvas);
                                  });
            self.menus.draw_popup(&mut canvas);
            self.app_menu.draw_popup(&mut canvas);
            self.redraw = false;
            self.content_damage.clear();
            crate::trace::record(crate::trace::RENDER,
                                 caller,
                                 started,
                                 crate::trace::clock().wrapping_sub(started),
                                 updating as u64,
                                 self.revision as u64);
        }
        else if !self.content_damage.is_empty()
        {
            let started = crate::trace::clock();
            let damage = core::mem::take(&mut self.content_damage);
            let fonts = crate::kernel::fonts();
            let cdef = self.cdef;
            let cache = &mut self.content_cache;
            let mut canvas = Canvas::new(platform::back_buffer(), 640, 480);
            for (id, region) in damage
            {
                if id.0 == mac_abi::quickdraw::DESKTOP_TARGET
                {
                    if self.processes
                           .get(crate::context::finder_app().0)
                           .is_ok_and(|p| p.info.hidden == 0)
                    {
                        self.windows.for_each_desktop_rect(self.desktop.bounds(), |visible| {
                            for band in region.bands() { for span in &band.spans {
                                let mut clip = Rect::ZERO;
                                if !super::quickdraw::geometry::sect_rect(Rect::new(span.left, band.top, span.right, band.bottom), visible, &mut clip) { continue; }
                                canvas.reset_port(); canvas.set_clip(clip);
                                background(&mut canvas);
                                cache.paint(id.0, self.desktop.bounds(), &mut canvas, |canvas| {
                                    crate::kernel::with_resource_owner(crate::context::finder_app().0, || self.desktop.paint(canvas, fonts, |canvas, command| paint(canvas, command, cdef)));
                                });
                                damaged.push(clip);
                            }}
                        });
                    }
                    continue;
                }
                let Some(window) = self.windows.window(id)
                else
                {
                    continue;
                };
                let Some(content) = self.content.iter().find(|content| content.id == id)
                else
                {
                    continue;
                };
                let has_viewports =
                    content.iter_commands()
                           .any(|command| matches!(command, RenderCommand::ScrollBox(_)));
                self.windows.for_each_visible_rect(id, |visible| {
                    for band in region.bands()
                    {
                        for span in &band.spans
                        {
                            let mut bounds = Rect::new(span.left, band.top, span.right, band.bottom);
                            bounds.offset(window.bounds.left, window.bounds.top);
                            let mut clip = Rect::ZERO;
                            if !super::quickdraw::geometry::sect_rect(bounds, visible, &mut clip) { continue; }
                            canvas.reset_port();
                            canvas.set_clip(clip);
                            canvas.set_origin(window.bounds.left, window.bounds.top);
                            let replay=|canvas:&mut Canvas<'_>| {
                                canvas.paint_rect(window.local_bounds(), WHITE);
                                crate::kernel::with_resource_owner(u64::from(window.refcon), || {
                                    content.paint(canvas, fonts, |canvas, command| paint(canvas, command, cdef));
                                });
                            };
                            if has_viewports {replay(&mut canvas);}else{cache.paint(id.0,window.bounds,&mut canvas,replay);}
                            window.paint_content_overlay(&mut canvas);
                            damaged.push(clip);
                        }
                    }
                });
            }
            crate::trace::record(crate::trace::RENDER,
                                 caller,
                                 started,
                                 crate::trace::clock().wrapping_sub(started),
                                 0,
                                 self.revision as u64);
        }
        if full || self.cursor_dirty || !damaged.is_empty()
        {
            self.compose_cursor(full, damaged);
        }
        self.publish_observation();
        self.enter(caller);
    }

    /// Timer-owned presentation never enters application drawing or replays a
    /// scene. It remains available when every app thread is CPU bound.
    fn present_cursor_overlay(&mut self)
    {
        if !self.cursor_dirty && self.outline_damage.is_empty()
        {
            return;
        }
        let caller = self.owner;
        self.enter(self.foreground);
        let damage = core::mem::take(&mut self.outline_damage);
        self.compose_cursor(false, damage);
        self.publish_observation();
        self.enter(caller);
    }

    fn compose_cursor(&mut self, full: bool, mut damaged: Vec<Rect>)
    {
        let text_cursor = self.text_cursor_at_mouse().then_some(self.text_cursor);
        let cursor =
            if self.responsiveness.active() { &mut self.busy_cursor } else { &mut self.cursors };
        let text_cursor = if self.responsiveness.active() { None } else { text_cursor };
        let bounds = cursor.bounds(self.events.mouse, text_cursor);
        let mut canvas = Canvas::new(platform::back_buffer(), 640, 480);
        let desktop_outline =
            (self.foreground == crate::context::finder_app().0
             && self.windows.active_window().is_none()).then(|| self.desktop.outline())
                                                       .flatten();
        let drag_outline = self.drag.feedback();
        let drag_highlight = self.drag.highlight();
        let outline_visible = !self.menus.is_tracking()
                              && !self.app_menu.is_tracking()
                              && !self.windows.is_tracking()
                              && (drag_highlight.is_some()
                                  || drag_outline.is_some()
                                  || desktop_outline.is_some()
                                  || self.windows
                                         .active_window()
                                         .and_then(|id| self.windows.window(id))
                                         .is_some_and(|window| window.outline_rect().is_some()));
        if outline_visible
        {
            use super::quickdraw::region::Region;
            let damage = if full
            {
                Region::from_rect(Rect::new(0, 0, 640, 480))
            }
            else
            {
                Region::from_rectangles(damaged.iter().copied().chain([self.cursor_bounds, bounds]))
            };
            if let Some((window, rect)) = drag_highlight
            {
                self.drag_highlight_under
                    .paint_highlight(&mut canvas, &self.windows, window, rect, &damage);
            }
            if let Some(outline) = drag_outline
            {
                self.outline_under.paint_screen(&mut canvas, outline, &damage);
            }
            else
            {
                self.outline_under.paint(&mut canvas, &self.windows, desktop_outline, &damage);
            }
        }
        let under = super::quickdraw::overlay::CursorUnder::capture(&canvas, bounds);
        cursor.draw_auto(&mut canvas, self.events.mouse, text_cursor);
        if full
        {
            platform::present(canvas.pixels);
        }
        else
        {
            platform::present_rects(canvas.pixels,
                                    damaged.iter().copied().chain([self.cursor_bounds, bounds]));
        }
        under.restore(&mut canvas);
        if outline_visible
        {
            self.outline_under.restore(&mut canvas);
            self.drag_highlight_under.restore(&mut canvas);
        }
        if self.outline_damage.is_empty() && self.outline_damage.capacity() < damaged.capacity()
        {
            damaged.clear();
            self.outline_damage = damaged;
        }
        self.cursor_bounds = bounds;
        self.cursor_dirty = false;
        self.presentations = self.presentations.wrapping_add(1);
    }

    fn publish_observation(&mut self)
    {
        if self.last_observed == (self.activity, self.revision, self.presentations)
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
                                             self.menu_cache.paints,
                                             self.presentations,
                                             self.compositions]);
        self.last_observed = (self.activity, self.revision, self.presentations);
    }
}

fn paint(canvas: &mut Canvas, command: &DrawCommand, cdef: DefinitionEntry)
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
            let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(), kind: u32::from_be_bytes(*b"CDEF"), id: 0, variation: 0, message: 0, reserved: 0, param: 0, object: (&mut state as *mut crate::toolbox::controls::ControlDefinitionState).cast(), gate: crate::machine::traps::gateway, surface: &mut surface };
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

impl super::classic::Services for Desktop
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
            self.legacy.font = selection;
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
        let status = fonts_service::query(fonts, &mut call, text);
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
        let mut call = mac_abi::quickdraw::FontCall { operation: mac_abi::quickdraw::FONT_DRAW,
                                                      window: id.0,
                                                      selection,
                                                      baseline: content.port.state.pen_location,
                                                      color: content.port.state.foreground,
                                                      ..Default::default() };
        self.font_draw(&mut call, text, crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)?;
        let content = self.content_mut(id.0)?;
        content.port.state.pen_location.h =
            content.port.state.pen_location.h.wrapping_add(call.width);
        Ok(())
    }

    fn cursor(&mut self, call: &mut mac_abi::quickdraw::CursorCall) -> Result<(), Status>
    {
        Desktop::cursor(self, call, &[])
    }

    fn ticks(&self) -> u32
    {
        Self::when()
    }
}
