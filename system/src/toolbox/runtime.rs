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

#[path = "dialogs/runtime.rs"]
mod dialog_runtime;
#[path = "events/runtime.rs"]
mod event_runtime;
#[path = "textedit/runtime.rs"]
mod textedit_runtime;
#[path = "processes/runtime.rs"]
mod local;
const MAX_RETAINED_BYTES: usize = 2 * 1024 * 1024;
pub struct Desktop
{
    dialogs:         super::dialogs::DialogState,
    desktop:         DesktopLayer,
    menus:           super::menus::Manager,
    cursors:         super::quickdraw::cursors::Cursors,
    worlds:          super::quickdraw::worlds::Worlds,
    classic_font:    mac_abi::quickdraw::FontSelection,
    legacy:          crate::context::LegacyGlobals,
    pub windows:     WindowManager,
    events:          Box<EventManager>,
    textedit:        super::textedit::Manager,
    definitions:     Definitions,
    content:         Vec<Content>,
    pending_content: Vec<Content>,
    decoders:        [Decoder; 2],
    pub revision:    u32,
    pub updates:     u32,
    last_key:        u8,
    last_action:     &'static str,
    redraw:          bool,
    last_logged:     u32,
    cdef:            DefinitionEntry,
    mdef:            DefinitionEntry,
    owner:           u64,
    foreground:      u64,
    locals:          Vec<local::Saved>,
    window_owners:   Vec<(WindowId, u64)>,
    processes:       super::processes::Manager,
    app_menu:        super::processes::menu::ApplicationMenu
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
            if self.owner != 1
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
        if id != mac_abi::quickdraw::DESKTOP_TARGET
           && !self.pending_content.iter().any(|content| content.id.0 == id)
        {
            self.redraw = true;
        }
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
        serial!("VIRTIO_INPUT=READY devices={}", devices);
        Ok(Self { dialogs: Default::default(),
                  menus: super::menus::Manager::new(mdef, crate::machine::traps::gateway),
                  desktop: DesktopLayer::new(Rect::new(0, 21, 640, 480)),
                  cursors: Default::default(),
                  worlds: Default::default(),
                  classic_font: Default::default(),
                  legacy:Default::default(),
                  windows: WindowManager::with_gate(crate::machine::traps::gateway),
                  events: Box::new(EventManager::new()),
                  textedit: Default::default(),
                  definitions,
                  content: Vec::new(),
                  pending_content: Vec::new(),
                  decoders: [Decoder::new(), Decoder::new()],
                  revision: 0,
                  updates: 0,
                  last_key: 0,
                  last_action: "READY",
                  redraw: true,
                  last_logged: u32::MAX,
                  cdef,mdef,owner:1,foreground:1,locals:alloc::vec![local::Saved::new(1,mdef)],window_owners:Vec::new(),
                  processes:super::processes::Manager::new(),
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
        if call.menu_id == super::processes::menu::MENU_ID
        {
            return self.app_menu.query(call).map_or_else(|e| e, |_| OK);
        }
        let result = self.menus.request(call, resources);
        if matches!(result, Ok(true))
        {
            self.redraw = true;
        }
        result.map_or_else(|status| status, |_| OK)
    }

    fn menu_selection(&mut self, selection: mac_abi::menus::Selection, when: u32)
    {
        self.events.post_system(Event { kind: EventKind::MenuCommand,
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
                request.id = id.0;
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
                self.content.push(Content { id,
                                            commands: Vec::new(),
                                            port: service::Port::default() });
                self.last_action = "NEW";
            }
            CLOSE_WINDOW =>
            {
                if !self.windows.dispose_window(id, &mut self.events, when)
                {
                    return NOT_FOUND;
                }
                self.content.retain(|content| content.id != id);
                self.pending_content.retain(|content| content.id != id);
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
                if self.owner != 1
                {
                    return ACCESS_DENIED;
                }
                if self.foreground != 1
                   && let Err(status) = self.activate_process_window(1, None)
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
                return OK;
            }
            INVALIDATE_WINDOW => self.windows.invalidate(id, request.bounds),
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
                let pending = Content { id,
                                        commands: Vec::new(),
                                        port: content.port.clone() };
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
                self.updates += 1;
            }
            END_UPDATE =>
            {
                let Some(index) = self.pending_content.iter().position(|content| content.id == id)
                else
                {
                    return BAD_ARGUMENT;
                };
                let Some(content) = self.content.iter_mut().find(|content| content.id == id)
                else
                {
                    return NOT_FOUND;
                };
                *content = self.pending_content.swap_remove(index);
                self.windows.end_update(id);
            }
            _ => return NOT_SUPPORTED
        }
        self.route_events();
        self.window_owners.retain(|(id, _)| self.windows.window(*id).is_some());
        self.revision = self.revision.wrapping_add(1);
        if operation != BEGIN_UPDATE
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

    pub fn control(&mut self, call: &mut mac_abi::controls::ControlCall) -> Status
    {
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
        content.commands.push(RenderCommand::Control(drawing));
        self.content_changed(call.window);
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

    pub fn graphics(&mut self, call: &mut mac_abi::quickdraw::GraphicsCall, points: &[Point],
                    region: &[u8])
                    -> Result<(), Status>
    {
        use mac_abi::quickdraw::{DESKTOP_TARGET, QD_DESKTOP_BEGIN, QD_DESKTOP_BOUNDS,
                                 QD_DESKTOP_CANCEL, QD_DESKTOP_END, QD_GET_STATE};
        if matches!(call.operation,
                    QD_DESKTOP_BEGIN | QD_DESKTOP_END | QD_DESKTOP_CANCEL | QD_DESKTOP_BOUNDS)
        {
            if self.owner != 1 && call.operation != QD_DESKTOP_BOUNDS
            {
                return Err(ACCESS_DENIED);
            }
            let committed = self.desktop.request(call)?;
            if committed
            {
                self.revision = self.revision.wrapping_add(1);
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
        self.cursors.request(call, colors, self.events.mouse)?;
        self.redraw = true;
        Ok(())
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
                                                    port:     content.port.clone() });
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
            Action::Close(id) =>
            {
                self.content.retain(|content| content.id != id);
                self.pending_content.retain(|content| content.id != id);
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
        self.revision = self.revision.wrapping_add(1);
        // Key delivery does not change window pixels. Repainting here stalls
        // dispatch on an obsolete frame before the application handles the key.
        // Pointer input can change the cursor, menu or window tracking state.
        if !matches!(input, Input::Key { .. })
        {
            self.redraw = true;
        }
        if self.dialogs.active()
        {
            self.dialog_input(input, when);
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
                        return;
                    }
                    if self.menus.begin(point)
                    {
                        return;
                    }
                    let hit = self.windows.find_window(point);
                    let active_before = self.windows.active_window();
                    let owner = hit.window.and_then(|id| self.window_owner(id)).unwrap_or(1);
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
                            self.windows
                                .start_tracking_with_modifiers(hit, point, self.events.modifiers);
                        }
                        else if hit.part == WindowPart::Content && active
                        {
                            let local = self.windows.window(id).unwrap().global_to_local(point);
                            let button = self.content.iter().find(|c| c.id == id).and_then(|c| c.commands.iter().filter_map(|command| match command { RenderCommand::Legacy(c)=>Some(c),_=>None }).find(|command| command.kind == DRAW_BUTTON && command.bounds.contains(local)));
                            if let Some(command) = button
                            {
                                let mut state = control(command);
                                let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(), kind: u32::from_be_bytes(*b"CDEF"), id: 0, variation: 0, message: 1, reserved: 0, param: (((local.v as u16 as u32) << 16) | local.h as u16 as u32) as i64, object: (&mut state as *mut crate::toolbox::controls::ControlDefinitionState).cast(), gate: crate::machine::traps::gateway, surface: core::ptr::null_mut() };
                                if super::resources::definitions::invoke(self.cdef, &mut call) > 0
                                {
                                    self.events.post_system(Event { kind: EventKind::OsEvent,
                                                                    message: 0x01000000 | id.0,
                                                                    when,
                                                                    where_: point,
                                                                    modifiers: self.events
                                                                                   .modifiers });
                                }
                            }
                            self.events.post_system(Event { kind: EventKind::MouseDown,
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
                        self.events.post_system(Event { kind: EventKind::MouseDown,
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
                    self.events.post_system(Event { kind: EventKind::MouseUp,
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
                if down && modifiers & CMD_KEY != 0 && self.menus.has_key(character)
                {
                    if let Some(selection) = self.menus.key(character)
                    {
                        self.menu_selection(selection, when);
                    }
                    return;
                }
                if mac_code != 0xff
                {
                    if down
                    {
                        self.last_key = character;
                    }
                    self.events.post_system(Event { kind: if !down
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

    fn poll_inputs(&mut self)
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
            return;
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
    }

    fn next_with_remove(&mut self, mask: u16, remove: bool) -> Event
    {
        self.poll_inputs();
        let when = Self::when();
        self.present();
        self.route_events();
        let mut synthetic = false;
        let event =
            (if remove
            {
                self.events.next_matching(mask)
            }
            else
            {
                self.events.peek_matching(mask)
            }).or_else(|| {
                  synthetic = true;
                  if mask & (1 << EventKind::Update as u16) != 0
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
                  }
              })
              .unwrap_or_else(|| self.events.null_event(when));
        let code = if event.kind == EventKind::Null
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
        event
    }

    pub fn present(&mut self)
    {
        crate::trace::record(crate::trace::PRESENT, self.owner, 0, 0, 0, 0);
        if !self.redraw && self.last_logged == self.revision
        {
            return;
        }
        let caller = self.owner;
        self.enter(self.foreground);
        if self.redraw
        {
            let started = crate::trace::clock();
            let updating = self.windows.back_to_front().filter(|w| w.updating).count();
            let back = platform::back_buffer();
            let mut canvas = Canvas::new(back, 640, 480);
            background(&mut canvas);
            let cdef = self.cdef;
            let fonts = crate::kernel::fonts();
            if self.processes.get(1).is_ok_and(|p| p.info.hidden == 0)
            {
                self.desktop
                    .paint(&mut canvas, fonts, |canvas, command| paint(canvas, command, cdef));
            }
            let content = &self.content;
            let cdef = self.cdef;
            self.windows.draw(&mut canvas, |window, canvas| {
                            canvas.paint_rect(window.local_bounds(), WHITE);
                            if let Some(content) = content.iter().find(|c| c.id == window.id)
                            {
                                crate::kernel::with_resource_owner(u64::from(window.refcon),||content.paint(canvas,fonts,|canvas,command|paint(canvas,command,cdef)));
                            }
                        });
            self.menus.draw(&mut canvas);
            self.app_menu.draw_bar(&mut canvas);
            self.app_menu.draw_popup(&mut canvas);
            self.cursors.draw(&mut canvas, self.events.mouse);
            platform::present(back);
            self.redraw = false;
            crate::trace::record(crate::trace::RENDER,
                                 caller,
                                 started,
                                 crate::trace::clock().wrapping_sub(started),
                                 updating as u64,
                                 self.revision as u64);
        }
        if self.last_logged != self.revision
        {
            self.last_logged = self.revision;
            let r = self.windows.window(WindowId(1)).map_or(Rect::ZERO, |w| w.bounds);
            serial!("WM_STATE rev={} action={} windows={} front={} key={} mouse={},{} \
                     first={},{},{},{} updates={} dropped={}",
                    self.revision,
                    self.last_action,
                    self.windows.count(),
                    self.windows.front_window().unwrap_or_default().0,
                    self.last_key,
                    self.events.mouse.h,
                    self.events.mouse.v,
                    r.left,
                    r.top,
                    r.right,
                    r.bottom,
                    self.updates,
                    self.events.dropped);
        }
        self.enter(caller);
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
    canvas.fill_rect(Rect::new(0, 0, 640, 480),
                     [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55],
                     0x878787,
                     0xc0c0c0);
}

impl super::classic::Services for Desktop
{
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
