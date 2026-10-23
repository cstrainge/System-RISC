//! Small native trap transport. There are no manager implementations in this crate.
#![no_std]
use mac_abi::{traps::*, *};

mod error;
pub mod types;
pub mod coordinates;
pub use error::Error;
pub use types::{Cnid, DocumentBatchId, FileTransferId, MenuId, ProcessId, ThreadId, TypeCode,
                VolumeId};
pub type Result<T> = core::result::Result<T, Error>;
mod files;
mod images;
pub fn status(value: i64) -> Result<()>
{
    if value == 0 { Ok(()) } else { Err(Error::from_status(value as Status)) }
}

/// Thread-mobile routing view into synchronized resident services. Copying a
/// client grants no object ownership or caller identity: System derives those
/// from its trusted execution context. Thread-specific operations may return
/// `Error::WrongThread`; mutable manager state never lives in this view.
#[derive(Clone, Copy)]
pub struct Client<'a>
{
    services: &'a SystemServices
}
impl core::fmt::Debug for Client<'_>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str("Client")
    }
}
impl PartialEq for Client<'_>
{
    fn eq(&self, other: &Self) -> bool
    {
        core::ptr::eq(self.services, other.services)
    }
}
impl Eq for Client<'_>
{
}
impl<'a> Client<'a>
{
    pub fn inspection(self, call: &mut mac_abi::inspection::Call) -> Result<()>
    {
        status(self.invoke(INSPECTION, call))
    }

    pub fn theme(self) -> Result<mac_abi::theme::Palette>
    {
        let mut palette = mac_abi::theme::Palette::default();
        status(self.invoke(THEME, &mut palette))?;
        Ok(palette)
    }

    /// Explicit original A-line transport for translated 68k applications/tests.
    /// The checked System adapter reads only this guest-memory slice.
    pub fn classic(self, instruction: u16, registers: &mut ClassicRegisters, guest_base: u32,
                   memory: &mut [u8])
                   -> Result<()>
    {
        let mut call = ClassicCall { instruction,
                                     reserved: 0,
                                     guest_base,
                                     memory: memory.as_mut_ptr(),
                                     memory_length: memory.len(),
                                     registers: *registers };
        let result = self.invoke(CLASSIC, &mut call);
        *registers = call.registers;
        status(result)
    }

    /// Lend binary flavor storage for this call only. Managers retain owned data.
    pub fn drag(self, call: &mut drag::Call, input: &[u8], output: &mut [u8]) -> Result<()>
    {
        call.input = input.as_ptr();
        call.length = input.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = self.invoke(DRAG, call);
        call.input = core::ptr::null();
        call.output = core::ptr::null_mut();
        call.length = 0;
        call.capacity = 0;
        status(result)
    }

    /// Borrow UTF-8 and output storage for one resident panic-report request.
    pub fn crash(self, call: &mut crashes::CrashCall, input: &str, output: &mut [u8])
                 -> Result<()>
    {
        call.data = input.as_ptr();
        call.length = input.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = self.invoke(CRASH, call);
        call.data = core::ptr::null();
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        status(result)
    }

    pub fn standard_file(self, call: &mut mac_abi::standard_file::StandardFileCall) -> Result<()>
    {
        status(self.invoke(mac_abi::traps::STANDARD_FILE, call))
    }

    pub fn dialog(self, call: &mut mac_abi::dialogs::DialogCall) -> Result<()>
    {
        status(self.invoke(DIALOG, call))
    }

    pub fn now(self) -> Result<dates::Timestamp>
    {
        let mut call = dates::ClockCall::default();
        status(self.invoke(CLOCK, &mut call))?;
        if call.reserved != 0
        {
            return Err(Error::BadAbi);
        }
        dates::Timestamp::from_unix_parts(call.unix_seconds, call.microseconds).ok_or(Error::BadAbi)
    }

    pub fn monotonic_nanoseconds(self) -> Result<u64>
    {
        let mut call = events::MonotonicCall::default();
        status(self.invoke(MONOTONIC_CLOCK, &mut call))?;
        Ok(call.nanoseconds)
    }

    pub fn menu(self, call: &mut mac_abi::menus::MenuCall) -> Result<()>
    {
        status(unsafe {
            (self.services.trap)(MENU,
                                 (call as *mut mac_abi::menus::MenuCall).cast(),
                                 core::mem::size_of::<mac_abi::menus::MenuCall>())
        })
    }

    pub fn new(services: &'a SystemServices) -> Result<Self>
    {
        if !services.header.supports::<SystemServices>()
        {
            return Err(Error::BadAbi);
        }
        Ok(Self { services })
    }

    /// # Safety
    /// Pointer must expose a readable Header and the advertised initialized table,
    /// whose callbacks and storage remain valid for lifetime 'a.
    pub unsafe fn from_raw(services: *const SystemServices) -> Result<Self>
    {
        if services.is_null()
        {
            return Err(Error::BadArgument);
        }
        if !unsafe { &*services.cast::<Header>() }.supports::<SystemServices>()
        {
            return Err(Error::BadAbi);
        }
        Self::new(unsafe { &*services })
    }

    pub fn capabilities(self) -> u64
    {
        self.services.capabilities
    }

    fn invoke<T>(self, number: u32, request: &mut T) -> i64
    {
        unsafe {
            (self.services.trap)(number, (request as *mut T).cast(), core::mem::size_of::<T>())
        }
    }

    pub fn log(self, text: &str) -> Result<()>
    {
        status(self.invoke(LOG,
                           &mut LogCall { bytes:  text.as_ptr(),
                                          length: text.len() }))
    }

    pub fn window(self, operation: u32, request: WindowRequest) -> Result<WindowRequest>
    {
        let mut call = WindowCall { operation,
                                    request };
        status(self.invoke(WINDOW, &mut call))?;
        Ok(call.request)
    }

    pub fn draw(self, window: u32, command: DrawCommand) -> Result<()>
    {
        status(self.invoke(DRAW,
                           &mut DrawCall { window,
                                           command }))
    }

    pub fn next_event(self, mask: u16) -> Result<Event>
    {
        let mut call = NextEventCall { mask,
                                       event: Event::default() };
        status(self.invoke(NEXT_EVENT, &mut call))?;
        Ok(call.event)
    }

    pub fn wait_event(self, mask: u16, timeout_ticks: u32) -> Result<Event>
    {
        let mut call = WaitEventCall { mask,
                                       timeout_ticks,
                                       ..Default::default() };
        status(self.invoke(WAIT_EVENT, &mut call))?;
        if call.waited != 0 { self.next_event(mask) } else { Ok(call.event) }
    }

    /// Additive event transport; original event requests keep their layouts.
    pub fn positioned_event(self, call: &mut events::PositionedEventCall) -> Result<()>
    {
        status(self.invoke(POSITIONED_EVENT, call))?;
        if call.waited != 0
        {
            call.operation = events::POSITION_NEXT;
            call.timeout_ticks = 0;
            status(self.invoke(POSITIONED_EVENT, call))?;
        }
        Ok(())
    }

    pub fn event(self, call: &mut events::EventCall) -> Result<()>
    {
        status(self.invoke(EVENTS, call))
    }

    /// Borrow clipboard buffers for one synchronous service call.
    pub fn scrap(self, call: &mut scrap::ScrapCall, input: &[u8], output: &mut [u8]) -> Result<()>
    {
        call.input = input.as_ptr();
        call.input_length = input.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = self.invoke(SCRAP, call);
        call.input = core::ptr::null();
        call.input_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        status(result)
    }

    /// Borrow text and scratch storage for one resident TextEdit request.
    pub fn textedit(self, call: &mut textedit::TextEditCall, input: &str, output: &mut [u8])
                    -> Result<()>
    {
        call.input = input.as_ptr();
        call.input_length = input.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = self.invoke(TEXTEDIT, call);
        call.input = core::ptr::null();
        call.input_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        status(result)
    }

    pub fn file_scan(self, call: &mut mac_abi::files::FileScanCall) -> Result<()>
    {
        status(self.invoke(FILE, call))
    }

    pub fn textedit_display(self, call: &mut textedit::TextEditDisplayCall) -> Result<()>
    {
        status(self.invoke(TEXTEDIT, call))
    }

    pub fn textedit_font(self, call: &mut textedit::TextEditFontCall) -> Result<()>
    {
        status(self.invoke(TEXTEDIT, call))
    }

    pub fn geometry(self, call: &mut GeometryCall) -> Result<()>
    {
        status(self.invoke(GEOMETRY, call))
    }

    /// Borrow bytes for one Memory Manager call, replacing all raw pointer fields.
    pub fn memory(self, call: &mut memory::MemoryCall, input: &[u8], output: &mut [u8])
                  -> Result<()>
    {
        call.input = input.as_ptr();
        call.input_length = input.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = self.invoke(MEMORY, call);
        call.input = core::ptr::null();
        call.input_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        status(result)
    }

    /// # Safety
    /// The definition-specific object and drawing surface must be valid/exclusive
    /// for the supplied resource's native contract throughout this call.
    pub unsafe fn definition(self, call: &mut DefinitionCall) -> Result<i64>
    {
        let result = self.invoke(DEFINITION, call);
        if result < 0 { Err(Error::from_status(result as Status)) } else { Ok(result) }
    }
}


#[derive(Default)]
pub struct SurfaceCommand<'a>
{
    pub operation:  types::SurfaceOperation,
    pub bounds:     Rect,
    pub foreground: u32,
    pub background: u32,
    pub pattern:    [u8; 8],
    pub font:       types::SurfaceFont,
    pub x:          i16,
    pub y:          i16,
    pub limit:      i16,
    pub text:       &'a str
}
pub struct SurfaceReply
{
    pub width:   i16,
    pub ascent:  i16,
    pub descent: i16,
    pub leading: i16
}
pub struct SurfaceConnection<'a>
{
    surface: &'a mut Surface,
    gate:    TrapGate
}
impl<'a> SurfaceConnection<'a>
{
    /// # Safety
    /// Surface must borrow live, exclusive and sufficiently large pixel memory.
    /// The gate must implement the native surface service for this borrow.
    pub unsafe fn new(surface: &'a mut Surface, gate: TrapGate) -> Self
    {
        Self { surface,
               gate }
    }

    pub fn perform(&mut self, command: SurfaceCommand<'_>) -> Result<SurfaceReply>
    {
        let mut call = SurfaceCall { operation:   command.operation as u32,
                                     surface:     self.surface,
                                     bounds:      command.bounds,
                                     foreground:  command.foreground,
                                     background:  command.background,
                                     pattern:     command.pattern,
                                     font:        command.font as u32,
                                     x:           command.x,
                                     y:           command.y,
                                     limit:       command.limit,
                                     reserved:    0,
                                     text:        command.text.as_ptr(),
                                     text_length: command.text.len(),
                                     width:       0,
                                     ascent:      0,
                                     descent:     0,
                                     leading:     0 };
        let value = unsafe {
            (self.gate)(SURFACE,
                        (&mut call as *mut SurfaceCall).cast(),
                        core::mem::size_of::<SurfaceCall>())
        };
        status(value)?;
        Ok(SurfaceReply { width:   call.width,
                          ascent:  call.ascent,
                          descent: call.descent,
                          leading: call.leading })
    }
}

/// Borrowed drawing request. The transport owns all raw pointer conversion.
#[derive(Default)]
pub struct GraphicsCommand<'a>
{
    pub operation:   u32,
    pub shape:       u32,
    pub verb:        u32,
    pub bounds:      Rect,
    pub point:       Point,
    pub corner:      Point,
    pub start_angle: i16,
    pub arc_angle:   i16,
    pub points:      &'a [Point],
    pub region:      &'a [u8]
}
fn graphics_call(window: u32, surface: *mut Surface, state: quickdraw::PortState,
                 command: GraphicsCommand<'_>)
                 -> quickdraw::GraphicsCall
{
    quickdraw::GraphicsCall { operation: command.operation,
                              window,
                              surface,
                              state,
                              shape: command.shape,
                              verb: command.verb,
                              bounds: command.bounds,
                              point: command.point,
                              corner: command.corner,
                              start_angle: command.start_angle,
                              arc_angle: command.arc_angle,
                              points: command.points.as_ptr(),
                              point_count: command.points.len(),
                              region: command.region.as_ptr(),
                              region_length: command.region.len() }
}

#[derive(Clone, Copy, Debug)]
pub struct FontReply
{
    pub selection:   quickdraw::FontSelection,
    pub index:       u32,
    pub name:        [u8; mac_abi::INLINE_STRING_BYTES],
    pub name_length: usize,
    pub width:       i16,
    pub metrics:     quickdraw::FontMetrics
}
impl FontReply
{
    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..self.name_length.min(self.name.len())]).unwrap_or("")
    }
}
fn font_reply(call: &quickdraw::FontCall) -> FontReply
{
    FontReply { selection:   call.selection,
                index:       call.index,
                name:        call.name,
                name_length: call.name_length as usize,
                width:       call.width,
                metrics:     call.metrics }
}
fn font_call(operation: u32, target: (u32, *mut Surface), selection: quickdraw::FontSelection,
             text: &str, index: u32, baseline: Point, color: u32)
             -> quickdraw::FontCall
{
    quickdraw::FontCall { operation,
                          window: target.0,
                          surface: target.1,
                          selection,
                          text: text.as_ptr(),
                          text_length: text.len(),
                          baseline,
                          color,
                          index,
                          ..Default::default() }
}

#[derive(Default)]
pub struct RegionCommand<'a>
{
    pub operation: u32,
    pub a:         &'a [u8],
    pub b:         &'a [u8],
    pub bounds:    Rect,
    pub point:     Point,
    pub points:    &'a [Point]
}
impl Client<'_>
{
    pub fn graphics(self, window: u32, state: &mut quickdraw::PortState,
                    command: GraphicsCommand<'_>)
                    -> Result<()>
    {
        let mut call = graphics_call(window, core::ptr::null_mut(), *state, command);
        status(self.invoke(QUICKDRAW, &mut call))?;
        *state = call.state;
        Ok(())
    }

    /// Control the retained onscreen desktop through the normal QuickDraw gate.
    /// The returned bounds use screen coordinates and exclude System's menu bar.
    pub fn desktop(self, operation: u32) -> Result<Rect>
    {
        let mut call = quickdraw::GraphicsCall { operation,
                                                 window: quickdraw::DESKTOP_TARGET,
                                                 ..Default::default() };
        status(self.invoke(QUICKDRAW, &mut call))?;
        Ok(call.bounds)
    }

    pub fn font(self, operation: u32, selection: quickdraw::FontSelection, text: &str, index: u32)
                -> Result<FontReply>
    {
        let mut call = font_call(operation,
                                 (0, core::ptr::null_mut()),
                                 selection,
                                 text,
                                 index,
                                 Point::default(),
                                 0);
        status(self.invoke(FONT, &mut call))?;
        Ok(font_reply(&call))
    }

    pub fn font_draw(self, window: u32, selection: quickdraw::FontSelection, baseline: Point,
                     text: &str, color: u32)
                     -> Result<i16>
    {
        let mut call = font_call(quickdraw::FONT_DRAW,
                                 (window, core::ptr::null_mut()),
                                 selection,
                                 text,
                                 0,
                                 baseline,
                                 color);
        status(self.invoke(FONT, &mut call))?;
        Ok(call.width)
    }

    pub fn region(self, command: RegionCommand<'_>, output: &mut [u8]) -> Result<(usize, bool)>
    {
        let mut call = quickdraw::RegionCall { operation: command.operation,
                                               a: command.a.as_ptr(),
                                               a_length: command.a.len(),
                                               b: command.b.as_ptr(),
                                               b_length: command.b.len(),
                                               points: command.points.as_ptr(),
                                               point_count: command.points.len(),
                                               bounds: command.bounds,
                                               point: command.point,
                                               output: output.as_mut_ptr(),
                                               capacity: output.len(),
                                               ..Default::default() };
        status(self.invoke(REGION, &mut call))?;
        Ok((call.required, call.result != 0))
    }
}
impl SurfaceConnection<'_>
{
    pub fn selected_font_metrics(&mut self, selection: quickdraw::FontSelection)
                                 -> Result<quickdraw::FontMetrics>
    {
        let mut call = font_call(quickdraw::FONT_METRICS,
                                 (0, self.surface),
                                 selection,
                                 "",
                                 0,
                                 Point::default(),
                                 0);
        status(unsafe {
            (self.gate)(FONT,
                        (&mut call as *mut quickdraw::FontCall).cast(),
                        core::mem::size_of::<quickdraw::FontCall>())
        })?;
        Ok(call.metrics)
    }

    pub fn graphics(&mut self, state: &mut quickdraw::PortState, command: GraphicsCommand<'_>)
                    -> Result<()>
    {
        let mut call = graphics_call(0, self.surface, *state, command);
        status(unsafe {
            (self.gate)(QUICKDRAW,
                        (&mut call as *mut quickdraw::GraphicsCall).cast(),
                        core::mem::size_of::<quickdraw::GraphicsCall>())
        })?;
        *state = call.state;
        Ok(())
    }

    pub fn font_width(&mut self, selection: quickdraw::FontSelection, text: &str) -> Result<i16>
    {
        let mut call = font_call(quickdraw::FONT_WIDTH,
                                 (0, self.surface),
                                 selection,
                                 text,
                                 0,
                                 Point::default(),
                                 0);
        status(unsafe {
            (self.gate)(FONT,
                        (&mut call as *mut quickdraw::FontCall).cast(),
                        core::mem::size_of::<quickdraw::FontCall>())
        })?;
        Ok(call.width)
    }

    pub fn font_draw(&mut self, selection: quickdraw::FontSelection, baseline: Point, text: &str,
                     color: u32)
                     -> Result<i16>
    {
        let mut call =
            font_call(quickdraw::FONT_DRAW, (0, self.surface), selection, text, 0, baseline, color);
        status(unsafe {
            (self.gate)(FONT,
                        (&mut call as *mut quickdraw::FontCall).cast(),
                        core::mem::size_of::<quickdraw::FontCall>())
        })?;
        Ok(call.width)
    }

    /// Preserve the borrowed surface's global clip while limiting font ink to
    /// the requested horizontal span, including partially visible glyphs.
    pub fn font_draw_clipped(&mut self, selection: quickdraw::FontSelection, baseline: Point,
                             text: &str, limit: i16, color: u32)
                             -> Result<i16>
    {
        let saved = self.surface.clip;
        let left = i32::from(baseline.h) + i32::from(self.surface.origin.h);
        let right = left + i32::from(limit.max(0));
        self.surface.clip.left =
            saved.left.max(left.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        self.surface.clip.right =
            saved.right.min(right.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        let result = self.font_draw(selection, baseline, text, color);
        self.surface.clip = saved;
        result
    }
}

pub enum BitmapData<'a>
{
    Bytes(&'a [u8]),
    Pixels(&'a [u32])
}
impl BitmapData<'_>
{
    fn raw(&self) -> (*const u8, usize)
    {
        match self
        {
            Self::Bytes(s) => (s.as_ptr(), s.len()),
            Self::Pixels(s) => (s.as_ptr().cast(), core::mem::size_of_val(*s))
        }
    }
}
pub struct BitmapCommand<'a>
{
    pub format:           u32,
    pub row_bytes:        usize,
    pub bounds:           Rect,
    pub source_rect:      Rect,
    pub destination_rect: Rect,
    pub data:             BitmapData<'a>,
    pub palette:          &'a [u32],
    pub mask:             &'a [u8],
    pub mode:             i16,
    pub foreground:       u32,
    pub background:       u32
}
fn bitmap_call(window: u32, surface: *mut Surface, c: BitmapCommand<'_>) -> quickdraw::BitmapCall
{
    quickdraw::BitmapCall { window,
                            surface,
                            format: c.format,
                            row_bytes: c.row_bytes,
                            bounds: c.bounds,
                            source_rect: c.source_rect,
                            destination_rect: c.destination_rect,
                            data: c.data.raw().0,
                            data_length: c.data.raw().1,
                            palette: c.palette.as_ptr(),
                            palette_length: c.palette.len(),
                            mask: c.mask.as_ptr(),
                            mask_length: c.mask.len(),
                            mode: c.mode,
                            reserved: 0,
                            foreground: c.foreground,
                            background: c.background }
}
impl Client<'_>
{
    pub fn copy_bits(self, window: u32, command: BitmapCommand<'_>) -> Result<()>
    {
        status(self.invoke(COPY_BITS, &mut bitmap_call(window, core::ptr::null_mut(), command)))
    }
}
impl SurfaceConnection<'_>
{
    pub fn copy_bits(&mut self, command: BitmapCommand<'_>) -> Result<()>
    {
        let mut call = bitmap_call(0, self.surface, command);
        status(unsafe {
            (self.gate)(COPY_BITS,
                        (&mut call as *mut quickdraw::BitmapCall).cast(),
                        core::mem::size_of::<quickdraw::BitmapCall>())
        })
    }
}

impl Client<'_>
{
    pub fn world(self, operation: u32, world: u32, dimensions: Point, pixels: &mut [u32])
                 -> Result<quickdraw::WorldCall>
    {
        let mut call = quickdraw::WorldCall { operation,
                                              world,
                                              dimensions,
                                              pixels: pixels.as_mut_ptr(),
                                              pixel_length: pixels.len(),
                                              ..Default::default() };
        status(self.invoke(WORLD, &mut call))?;
        Ok(call)
    }

    pub fn world_copy(self, world: u32, target: u32, source: Rect, destination: Rect, mode: i16)
                      -> Result<()>
    {
        status(self.invoke(WORLD,
                           &mut quickdraw::WorldCall { operation: quickdraw::WORLD_COPY,
                                                       world,
                                                       target,
                                                       source,
                                                       destination,
                                                       mode,
                                                       ..Default::default() }))
    }
}
impl Client<'_>
{
    pub fn cursor_resource(self, mut call: quickdraw::CursorResourceCall)
                           -> Result<quickdraw::CursorSnapshot>
    {
        status(self.invoke(CURSOR, &mut call))?;
        Ok(call.snapshot)
    }

    pub fn cursor(self, operation: u32, cursor: quickdraw::MonoCursor, bounds: Rect,
                  offset: Point, colors: &[u32])
                  -> Result<i32>
    {
        let mut call = quickdraw::CursorCall { operation,
                                               cursor,
                                               bounds,
                                               offset,
                                               colors: colors.as_ptr(),
                                               color_count: colors.len(),
                                               visibility: 0 };
        status(self.invoke(CURSOR, &mut call))?;
        Ok(call.visibility)
    }
}
impl Client<'_>
{
    pub fn map_point(self, point: Point, source: Rect, destination: Rect, scale: bool)
                     -> Result<Point>
    {
        let mut call = quickdraw::MappingCall { operation: if scale
                                                {
                                                    quickdraw::SCALE_POINT
                                                }
                                                else
                                                {
                                                    quickdraw::MAP_POINT
                                                },
                                                source,
                                                destination,
                                                point,
                                                ..Default::default() };
        status(self.invoke(MAPPING, &mut call))?;
        Ok(call.point)
    }

    pub fn map_rect(self, rect: Rect, source: Rect, destination: Rect) -> Result<Rect>
    {
        let mut call = quickdraw::MappingCall { operation: quickdraw::MAP_RECTANGLE,
                                                source,
                                                destination,
                                                rect,
                                                ..Default::default() };
        status(self.invoke(MAPPING, &mut call))?;
        Ok(call.rect)
    }

    pub fn map_points(self, points: &mut [Point], source: Rect, destination: Rect) -> Result<()>
    {
        let mut call = quickdraw::MappingCall { operation: quickdraw::MAP_POLYGON,
                                                source,
                                                destination,
                                                points: points.as_mut_ptr(),
                                                point_count: points.len(),
                                                ..Default::default() };
        status(self.invoke(MAPPING, &mut call))
    }
}

#[derive(Default)]
pub struct ResourceRequest<'a>
{
    pub operation: u32,
    pub scope:     i16,
    pub key:       ResourceKey,
    pub index:     u32,
    pub name:      &'a str,
    pub offset:    usize
}
pub struct ResourceReply
{
    pub info:        resources::ResourceInfo,
    pub index:       u32,
    pub kind:        [u8; 4],
    pub transferred: usize
}
impl Client<'_>
{
    pub fn file_resource(self, call: &mut resources::FileResourceCall, name: &str,
                         output: &mut [u8])
                         -> Result<()>
    {
        call.name = name.as_ptr();
        call.name_length = name.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = status(self.invoke(RESOURCE, call));
        call.name = core::ptr::null();
        call.name_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        result
    }

    pub fn file_version(self, volume: u32, cnid: u32, id: i16) -> Result<resources::VersionInfo>
    {
        let mut call = resources::FileVersionCall { operation: resources::FILE_VERSION,
                                                    volume,
                                                    cnid,
                                                    id,
                                                    ..Default::default() };
        status(self.invoke(RESOURCE, &mut call))?;
        Ok(call.info)
    }

    pub fn resource(self, request: ResourceRequest<'_>, output: &mut [u8])
                    -> Result<ResourceReply>
    {
        let mut call = resources::ResourceCall { operation: request.operation,
                                                 scope: request.scope,
                                                 key: request.key,
                                                 index: request.index,
                                                 name: request.name.as_ptr(),
                                                 name_length: request.name.len(),
                                                 output: output.as_mut_ptr(),
                                                 capacity: output.len(),
                                                 offset: request.offset,
                                                 ..Default::default() };
        status(self.invoke(RESOURCE, &mut call))?;
        Ok(ResourceReply { info:        call.info,
                           index:       call.index,
                           kind:        call.key.kind,
                           transferred: call.transferred })
    }
}
impl Client<'_>
{
    pub fn picture(self, window: u32, data: &[u8], destination: Rect) -> Result<()>
    {
        let mut call = quickdraw::PictureCall { operation: quickdraw::PICTURE_BYTES,
                                                window,
                                                data: data.as_ptr(),
                                                length: data.len(),
                                                destination,
                                                ..Default::default() };
        status(self.invoke(PICTURE, &mut call))
    }

    pub fn picture_resource(self, window: u32, scope: i16, id: i16, destination: Rect)
                            -> Result<()>
    {
        status(self.invoke(PICTURE,
                           &mut quickdraw::PictureCall { operation: quickdraw::PICTURE_RESOURCE,
                                                         window,
                                                         scope,
                                                         id,
                                                         destination,
                                                         ..Default::default() }))
    }

    pub fn icon(self, mut call: quickdraw::IconCall) -> Result<quickdraw::IconCall>
    {
        // IconCall only carries scalar identifiers and an optional surface pointer;
        // public window operations always clear that pointer before crossing ABI.
        call.surface = core::ptr::null_mut();
        status(self.invoke(ICON, &mut call))?;
        Ok(call)
    }

    pub fn file_icon(self, call: &mut quickdraw::FileIconCall) -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        status(self.invoke(ICON, call))
    }

    pub fn default_file_icon(self, call: &mut quickdraw::DefaultFileIconCall) -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        status(self.invoke(ICON, call))
    }
}
impl SurfaceConnection<'_>
{
    pub fn file_icon(&mut self, volume: u32, cnid: u32, destination: Rect, flags: u16)
                     -> Result<()>
    {
        let mut call = quickdraw::FileIconCall { operation: quickdraw::ICON_PLOT_FILE,
                                                 surface: self.surface,
                                                 volume,
                                                 cnid,
                                                 destination,
                                                 flags,
                                                 ..Default::default() };
        status(unsafe {
            (self.gate)(ICON,
                        core::ptr::from_mut(&mut call).cast(),
                        core::mem::size_of::<quickdraw::FileIconCall>())
        })
    }

    pub fn picture(&mut self, data: &[u8], destination: Rect) -> Result<()>
    {
        let mut call = quickdraw::PictureCall { operation: quickdraw::PICTURE_BYTES,
                                                surface: self.surface,
                                                data: data.as_ptr(),
                                                length: data.len(),
                                                destination,
                                                ..Default::default() };
        status(unsafe {
            (self.gate)(PICTURE,
                        (&mut call as *mut quickdraw::PictureCall).cast(),
                        core::mem::size_of::<quickdraw::PictureCall>())
        })
    }

    pub fn icon(&mut self, scope: i16, id: i16, destination: Rect, flags: u16) -> Result<()>
    {
        let mut call = quickdraw::IconCall { surface: self.surface,
                                             scope,
                                             id,
                                             destination,
                                             flags,
                                             ..Default::default() };
        status(unsafe {
            (self.gate)(ICON,
                        (&mut call as *mut quickdraw::IconCall).cast(),
                        core::mem::size_of::<quickdraw::IconCall>())
        })
    }
}

impl Client<'_>
{
    pub fn control(self, call: &mut mac_abi::controls::ControlCall) -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        status(self.invoke(CONTROL, call))
    }

    /// Draw a control into caller-owned storage for this synchronous service call.
    pub fn control_pixels(self, call: &mut mac_abi::controls::ControlCall, pixels: &mut [u32],
                          width: usize, height: usize)
                          -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        if width == 0
           || height == 0
           || width > i16::MAX as usize
           || height > i16::MAX as usize
           || width.checked_mul(height) != Some(pixels.len())
           || pixels.len() > 640 * 480
        {
            return Err(Error::BadArgument);
        }
        let mut surface = Surface { pixels: pixels.as_mut_ptr(),
                                    width:  width as u32,
                                    height: height as u32,
                                    origin: Point::default(),
                                    clip:   Rect::new(0, 0, width as i16, height as i16) };
        call.surface = &mut surface;
        let result = status(self.invoke(CONTROL, call));
        call.surface = core::ptr::null_mut();
        result
    }
}

impl Client<'_>
{
    pub fn document(self, call: &mut mac_abi::processes::DocumentCall) -> Result<()>
    {
        status(self.invoke(PROCESS, call))
    }

    pub fn process(self, call: &mut mac_abi::processes::ProcessCall) -> Result<()>
    {
        status(self.invoke(PROCESS, call))
    }
}

impl Client<'_>
{
    pub fn icon_data(self, call: &mut quickdraw::IconDataCall, data: &[u8]) -> Result<()>
    {
        call.data = data.as_ptr();
        call.length = data.len();
        status(self.invoke(ICON, call))
    }
}

impl Client<'_>
{
    pub fn fork_write(self, call: &mut mac_abi::files::ForkWriteCall, name: &str, comment: &str,
                      data: &[u8], output: &mut [u8])
                      -> Result<()>
    {
        call.name = name.as_ptr();
        call.name_length = name.len();
        call.comment = comment.as_ptr();
        call.comment_length = comment.len();
        call.data = data.as_ptr();
        call.count = data.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        let result = status(self.invoke(FILE, call));
        call.name = core::ptr::null();
        call.name_length = 0;
        call.comment = core::ptr::null();
        call.comment_length = 0;
        call.data = core::ptr::null();
        call.count = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        result
    }

    pub fn data_write(self, call: &mut mac_abi::files::DataWriteCall, name: &str, data: &[u8])
                      -> Result<()>
    {
        call.name = name.as_ptr();
        call.name_length = name.len();
        call.data = data.as_ptr();
        call.data_length = data.len();
        let result = status(self.invoke(FILE, call));
        call.name = core::ptr::null();
        call.name_length = 0;
        call.data = core::ptr::null();
        call.data_length = 0;
        result
    }
}

impl Client<'_>
{
    pub fn scroll_box(self, call: &mut mac_abi::scrollbox::Call) -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        status(self.invoke(SCROLL_BOX, call))
    }

    pub fn scroll_box_pixels(self, call: &mut mac_abi::scrollbox::Call, pixels: &mut [u32],
                             width: usize, height: usize)
                             -> Result<()>
    {
        call.surface = core::ptr::null_mut();
        if width == 0
           || height == 0
           || width > i16::MAX as usize
           || height > i16::MAX as usize
           || width.checked_mul(height) != Some(pixels.len())
           || pixels.len() > 640 * 480
        {
            return Err(Error::BadArgument);
        }
        let mut surface = Surface { pixels: pixels.as_mut_ptr(),
                                    width:  width as u32,
                                    height: height as u32,
                                    origin: Point::default(),
                                    clip:   Rect::new(0, 0, width as i16, height as i16) };
        call.surface = &mut surface;
        let result = status(self.invoke(SCROLL_BOX, call));
        call.surface = core::ptr::null_mut();
        result
    }
}
