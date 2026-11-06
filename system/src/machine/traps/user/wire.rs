//! Explicit native wire layouts. Only the listed scalar, array and C-layout
//! fields may become resident Rust values. Encoding never reads struct padding.
use core::{mem::size_of, ptr};

/// # Safety
/// validate must reject every invalid Rust bit pattern before a typed borrow.
/// encode reads only initialized fields and writes exactly size_of::<Self>().
/// Self contains no owning resources/references or Drop obligation: a validated
/// byte copy is an independent value, including when its storage is unaligned.
pub(crate) unsafe trait Wire: Sized
{
    fn validate(bytes: &[u8]) -> bool;
    unsafe fn encode(value: *const Self, output: *mut u8);
}
macro_rules! scalar {
    ($($ty:ty),* $(,)?) => {$(
        unsafe impl Wire for $ty {
            fn validate(bytes: &[u8]) -> bool { bytes.len() == size_of::<Self>() }
            unsafe fn encode(value: *const Self, output: *mut u8) {
                unsafe { ptr::copy_nonoverlapping(value.cast::<u8>(), output, size_of::<Self>()) };
            }
        }
    )*};
}
scalar!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);
unsafe impl<T> Wire for *const T
{
    fn validate(bytes: &[u8]) -> bool
    {
        bytes.len() == size_of::<Self>()
    }

    unsafe fn encode(value: *const Self, output: *mut u8)
    {
        unsafe { ptr::copy_nonoverlapping(value.cast::<u8>(), output, size_of::<Self>()) };
    }
}
unsafe impl<T> Wire for *mut T
{
    fn validate(bytes: &[u8]) -> bool
    {
        bytes.len() == size_of::<Self>()
    }

    unsafe fn encode(value: *const Self, output: *mut u8)
    {
        unsafe { ptr::copy_nonoverlapping(value.cast::<u8>(), output, size_of::<Self>()) };
    }
}
unsafe impl<T: Wire, const N: usize> Wire for [T; N]
{
    fn validate(bytes: &[u8]) -> bool
    {
        bytes.len() == size_of::<Self>()
        && (0..N).all(|i| T::validate(&bytes[i * size_of::<T>()..(i + 1) * size_of::<T>()]))
    }

    unsafe fn encode(value: *const Self, output: *mut u8)
    {
        for i in 0..N
        {
            unsafe { T::encode(value.cast::<T>().add(i), output.add(i * size_of::<T>())) };
        }
    }
}
macro_rules! wire_struct {
    ($ty:ty { $($field:tt : $field_ty:ty),* $(,)? }) => {
        unsafe impl $crate::machine::traps::user::wire::Wire for $ty {
            fn validate(bytes: &[u8]) -> bool {
                bytes.len() == core::mem::size_of::<Self>() $( && <$field_ty as $crate::machine::traps::user::wire::Wire>::validate(&bytes[core::mem::offset_of!($ty,$field)..core::mem::offset_of!($ty,$field)+core::mem::size_of::<$field_ty>()]))*
            }
            unsafe fn encode(value: *const Self, output: *mut u8) {
                unsafe { core::ptr::write_bytes(output, 0, core::mem::size_of::<Self>()) };
                $( unsafe { <$field_ty as $crate::machine::traps::user::wire::Wire>::encode(core::ptr::addr_of!((*value).$field), output.add(core::mem::offset_of!($ty,$field))) }; )*
            }
        }
    };
}
pub(crate) use wire_struct;

/// Encode exactly one initialized wire value without reading padding.
pub(crate) fn encode_value<T: Wire>(value: &T, output: &mut [u8]) -> Result<(), mac_abi::Status>
{
    if output.len() != size_of::<T>()
    {
        return Err(mac_abi::BAD_ARGUMENT);
    }
    unsafe { T::encode(core::ptr::from_ref(value), output.as_mut_ptr()) };
    Ok(())
}

/// Validate every field before creating an owned Rust value. Input alignment
/// is irrelevant; no reference into the borrowed wire buffer is returned.
pub(crate) fn decode_value<T: Wire>(bytes: &[u8]) -> Result<T, mac_abi::Status>
{
    if !T::validate(bytes)
    {
        return Err(mac_abi::BAD_ARGUMENT);
    }
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
}

// EventKind is a Rust enum, not an unrestricted u16. No typed Event exists
// until this check has also run for every nested input event.
unsafe impl Wire for mac_abi::events::EventKind
{
    fn validate(bytes: &[u8]) -> bool
    {
        bytes.len() == 2 && u16::from_ne_bytes([bytes[0], bytes[1]]) <= 15
    }

    unsafe fn encode(value: *const Self, output: *mut u8)
    {
        unsafe { ptr::copy_nonoverlapping(value.cast::<u8>(), output, 2) };
    }
}
wire_struct!(mac_abi::events::KeyMap { 0: [u8; 16] });
wire_struct!(mac_abi::WindowId { 0: u64 });
wire_struct!(mac_abi::textedit::EditorId { 0: u64 });
wire_struct!(mac_abi::traps::LogCall {
    bytes: *const u8,
    length: usize,
});

wire_struct!(mac_abi::geometry::Rect { top:    i16,
                                       left:   i16,
                                       bottom: i16,
                                       right:  i16 });

wire_struct!(mac_abi::WindowRequest { id:             u64,
                                      bounds:         mac_abi::geometry::Rect,
                                      title:          [u8; mac_abi::WINDOW_TITLE_BYTES],
                                      title_length:   u32,
                                      procedure:      i16,
                                      minimum_width:  i16,
                                      minimum_height: i16,
                                      reserved:       i16 });

wire_struct!(mac_abi::traps::WindowCall { operation: u32,
                                          request:   mac_abi::WindowRequest });

wire_struct!(mac_abi::DrawCommand { kind:        u32,
                                    bounds:      mac_abi::geometry::Rect,
                                    color:       u32,
                                    text_length: u32,
                                    text:        [u8; mac_abi::INLINE_STRING_BYTES] });

wire_struct!(mac_abi::traps::DrawCall { window:  u64,
                                        command: mac_abi::DrawCommand });

wire_struct!(mac_abi::geometry::Point { v: i16,
                                        h: i16 });

wire_struct!(mac_abi::events::Event { kind:      mac_abi::events::EventKind,
                                      message:   u32,
                                      when:      u32,
                                      where_:    mac_abi::geometry::Point,
                                      modifiers: u16,
                                      payload:   u64,
                                      window:    u64 });

wire_struct!(mac_abi::traps::NextEventCall { mask:  u16,
                                             event: mac_abi::events::Event });

wire_struct!(mac_abi::traps::WaitEventCall { mask:          u16,
                                             timeout_ticks: u32,
                                             event:         mac_abi::events::Event,
                                             waited:        u32 });

wire_struct!(mac_abi::traps::ClassicRegisters { d:        [u32; 8],
                                                a:        [u32; 8],
                                                pc:       u32,
                                                sr:       u16,
                                                reserved: u16 });

wire_struct!(mac_abi::traps::ClassicCall {
    instruction: u16,
    reserved: u16,
    guest_base: u32,
    memory: *mut u8,
    memory_length: usize,
    registers: mac_abi::traps::ClassicRegisters,
});

wire_struct!(mac_abi::traps::GeometryCall { operation: u32,
                                            a:         mac_abi::geometry::Rect,
                                            b:         mac_abi::geometry::Rect,
                                            output:    mac_abi::geometry::Rect,
                                            result:    u32 });

wire_struct!(mac_abi::Surface {
    pixels: *mut u32,
    width: u32,
    height: u32,
    origin: mac_abi::geometry::Point,
    clip: mac_abi::geometry::Rect,
});

wire_struct!(mac_abi::traps::SurfaceCall {
    operation: u32,
    surface: *mut mac_abi::Surface,
    bounds: mac_abi::geometry::Rect,
    foreground: u32,
    background: u32,
    pattern: [u8; 8],
    font: u32,
    x: i16,
    y: i16,
    limit: i16,
    reserved: i16,
    text: *const u8,
    text_length: usize,
    width: i16,
    ascent: i16,
    descent: i16,
    leading: i16,
});

wire_struct!(mac_abi::quickdraw::Target { kind:     u32,
                                          reserved: u32,
                                          id:       u64 });

wire_struct!(mac_abi::quickdraw::PortState { location:           mac_abi::geometry::Point,
                                             pen_size:           mac_abi::geometry::Point,
                                             mode:               i16,
                                             visibility:         i16,
                                             pattern:            [u8; 8],
                                             background_pattern: [u8; 8],
                                             foreground:         u32,
                                             background:         u32,
                                             clip:               mac_abi::geometry::Rect,
                                             origin:             mac_abi::geometry::Point });

wire_struct!(mac_abi::quickdraw::GraphicsCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    state: mac_abi::quickdraw::PortState,
    shape: u32,
    verb: u32,
    bounds: mac_abi::geometry::Rect,
    point: mac_abi::geometry::Point,
    corner: mac_abi::geometry::Point,
    start_angle: i16,
    arc_angle: i16,
    points: *const mac_abi::geometry::Point,
    point_count: usize,
    region: *const u8,
    region_length: usize,
});

wire_struct!(mac_abi::quickdraw::FontSelection { family: i16,
                                                 size:   i16,
                                                 face:   u16 });

wire_struct!(mac_abi::quickdraw::FontMetrics { ascent:    i16,
                                               descent:   i16,
                                               leading:   i16,
                                               max_width: i16 });

wire_struct!(mac_abi::quickdraw::FontCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    selection: mac_abi::quickdraw::FontSelection,
    text: *const u8,
    text_length: usize,
    baseline: mac_abi::geometry::Point,
    color: u32,
    index: u32,
    name: [u8; mac_abi::INLINE_STRING_BYTES],
    name_length: u32,
    width: i16,
    metrics: mac_abi::quickdraw::FontMetrics,
});

wire_struct!(mac_abi::quickdraw::FontSuitcaseCall {
    operation: u32,
    reserved: u32,
    id: u64,
    data: *const u8,
    data_length: usize,
    resource: *const u8,
    resource_length: usize,
    index: u32,
    count: u32,
    expected_data: u32,
    expected_resource: u32,
    name: [u8; 256],
    name_length: u32,
});

wire_struct!(mac_abi::quickdraw::RegionCall {
    operation: u32,
    a: *const u8,
    a_length: usize,
    b: *const u8,
    b_length: usize,
    bounds: mac_abi::geometry::Rect,
    point: mac_abi::geometry::Point,
    points: *const mac_abi::geometry::Point,
    point_count: usize,
    output: *mut u8,
    capacity: usize,
    required: usize,
    result: u32,
});

wire_struct!(mac_abi::quickdraw::BitmapCall {
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    format: u32,
    row_bytes: usize,
    bounds: mac_abi::geometry::Rect,
    source_rect: mac_abi::geometry::Rect,
    destination_rect: mac_abi::geometry::Rect,
    data: *const u8,
    data_length: usize,
    palette: *const u32,
    palette_length: usize,
    mask: *const u8,
    mask_length: usize,
    mode: i16,
    reserved: u16,
    foreground: u32,
    background: u32,
});

wire_struct!(mac_abi::quickdraw::WorldCall {
    operation: u32,
    world: u64,
    target: mac_abi::quickdraw::Target,
    dimensions: mac_abi::geometry::Point,
    source: mac_abi::geometry::Rect,
    destination: mac_abi::geometry::Rect,
    mode: i16,
    reserved: u16,
    pixels: *mut u32,
    pixel_length: usize,
});

wire_struct!(mac_abi::quickdraw::MonoCursor { image:   [u16; 16],
                                              mask:    [u16; 16],
                                              hotspot: mac_abi::geometry::Point });

wire_struct!(mac_abi::quickdraw::CursorCall {
    operation: u32,
    cursor: mac_abi::quickdraw::MonoCursor,
    bounds: mac_abi::geometry::Rect,
    offset: mac_abi::geometry::Point,
    colors: *const u32,
    color_count: usize,
    visibility: i32,
});

wire_struct!(mac_abi::resources::Scope { kind:     u32,
                                         reserved: u32,
                                         image:    u64 });

wire_struct!(mac_abi::quickdraw::CursorSnapshot { cursor:      mac_abi::quickdraw::MonoCursor,
                                                  visibility:  i32,
                                                  flags:       u32,
                                                  frame_index: u32,
                                                  frame_count: u32,
                                                  interval:    u32,
                                                  resource_id: i16,
                                                  scope:       mac_abi::resources::Scope });

wire_struct!(mac_abi::quickdraw::CursorResourceCall { operation: u32,
                                                      scope:     mac_abi::resources::Scope,
                                                      id:        i16,
                                                      interval:  u32,
                                                      builtin:   u32,
                                                      snapshot:  mac_abi::quickdraw::CursorSnapshot });

wire_struct!(mac_abi::quickdraw::MappingCall {
    operation: u32,
    source: mac_abi::geometry::Rect,
    destination: mac_abi::geometry::Rect,
    point: mac_abi::geometry::Point,
    rect: mac_abi::geometry::Rect,
    points: *mut mac_abi::geometry::Point,
    point_count: usize,
});

wire_struct!(mac_abi::quickdraw::PictureCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    scope: mac_abi::resources::Scope,
    id: i16,
    data: *const u8,
    length: usize,
    destination: mac_abi::geometry::Rect,
    bounds: mac_abi::geometry::Rect,
    command_count: u32,
});

wire_struct!(mac_abi::quickdraw::IconCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    scope: mac_abi::resources::Scope,
    id: i16,
    destination: mac_abi::geometry::Rect,
    flags: u16,
    origin: mac_abi::resources::Scope,
    index: u32,
    creator: [u8; 4],
    file_type: [u8; 4],
});

wire_struct!(mac_abi::quickdraw::FileIconCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    volume: u32,
    cnid: u32,
    destination: mac_abi::geometry::Rect,
    flags: u16,
    reserved: [u16; 3],
});

wire_struct!(mac_abi::quickdraw::DefaultFileIconCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    surface: *mut mac_abi::Surface,
    file_type: [u8; 4],
    name_length: u32,
    name: [u8; 256],
    destination: mac_abi::geometry::Rect,
    flags: u16,
    reserved: u16,
});

wire_struct!(mac_abi::quickdraw::IconDataCall {
    target: mac_abi::quickdraw::Target,
    kind: [u8; 4],
    data: *const u8,
    length: usize,
    destination: mac_abi::geometry::Rect,
    surface: *mut mac_abi::Surface,
    reserved: [u64; 2],
});

wire_struct!(mac_abi::files::VolumeInfo { id:                 u32,
                                          disk:               u32,
                                          partition_start:    u64,
                                          total_bytes:        u64,
                                          free_bytes:         u64,
                                          created:            u64,
                                          modified:           u64,
                                          filesystem:         u32,
                                          filesystem_version: u32,
                                          blessed_folder:     u32,
                                          attributes:         u16,
                                          read_only:          u16,
                                          startup:            u32,
                                          icon_kind:          [u8; 4],
                                          icon_id:            i16,
                                          capacity_known:     u16,
                                          finder_info:        [u8; 32],
                                          name_length:        u32,
                                          name:               [u8; 256] });

wire_struct!(mac_abi::files::CatalogInfo { volume:          u32,
                                           id:              u32,
                                           parent:          u32,
                                           kind:            u32,
                                           data_length:     u64,
                                           resource_length: u64,
                                           file_type:       [u8; 4],
                                           creator:         [u8; 4],
                                           flags:           u16,
                                           child_count:     u32,
                                           created:         u64,
                                           modified:        u64,
                                           backup:          u64,
                                           backup_present:  u32,
                                           finder_info:     [u8; 32],
                                           name_length:     u32,
                                           name:            [u8; 256] });

wire_struct!(mac_abi::files::FileCall {
    operation: u32,
    file_type: [u8; 4],
    volume: u32,
    cnid: u32,
    index: u32,
    handle: u32,
    fork: u32,
    whence: u32,
    reserved: u32,
    open_flags: u32,
    offset: i64,
    position: u64,
    length: u64,
    name: *const u8,
    name_length: usize,
    input: *const u8,
    input_length: usize,
    output: *mut u8,
    capacity: usize,
    transferred: usize,
    volume_info: mac_abi::files::VolumeInfo,
    entry: mac_abi::files::CatalogInfo,
});

wire_struct!(mac_abi::files::DataWriteCall {
    operation: u32,
    handle: u64,
    volume: u32,
    parent: u32,
    expected_cnid: u32,
    when: u64,
    file_type: [u8; 4],
    creator: [u8; 4],
    length: usize,
    name: *const u8,
    name_length: usize,
    data: *const u8,
    data_length: usize,
    entry: mac_abi::files::CatalogInfo,
    reserved: [u64; 2],
});

wire_struct!(mac_abi::files::DirectoryScanCursor { revision:  u64,
                                                   volume:    u32,
                                                   directory: u32,
                                                   node:      u32,
                                                   previous:  u32,
                                                   index:     u32,
                                                   traversed: u32,
                                                   state:     u32,
                                                   reserved:  u32,
                                                   registry:  u32 });

wire_struct!(mac_abi::files::DesktopScanCursor { volume:    u32,
                                                 state:     u32,
                                                 directory: mac_abi::files::DirectoryScanCursor });

wire_struct!(mac_abi::files::FileScanCall { operation:          u32,
                                            volume:             u32,
                                            directory:          u32,
                                            step:               u32,
                                            cursor:             mac_abi::files::DirectoryScanCursor,
                                            desktop:            mac_abi::files::DesktopScanCursor,
                                            entry:              mac_abi::files::CatalogInfo,
                                            data_allocated:     u64,
                                            resource_allocated: u64 });

wire_struct!(mac_abi::files::PipeOpenCall { operation: u32,
                                            volume:    u32,
                                            cnid:      u32,
                                            reserved:  u32,
                                            writer:    u64,
                                            folder:    u32,
                                            reserved2: u32 });

wire_struct!(mac_abi::files::ForkWriteCall {
    operation: u32,
    handle: u64,
    volume: u32,
    parent: u32,
    expected_cnid: u32,
    fork: u32,
    when: u64,
    data_length: u64,
    resource_length: u64,
    created: u64,
    modified: u64,
    backup: u64,
    backup_present: u32,
    attributes: u16,
    padding: u16,
    finder_info: [u8; 32],
    name: *const u8,
    name_length: usize,
    comment: *const u8,
    comment_length: usize,
    data: *const u8,
    count: usize,
    output: *mut u8,
    capacity: usize,
    offset: u64,
    transferred: usize,
    completed: u32,
    reserved: u32,
    entry: mac_abi::files::CatalogInfo,
});

wire_struct!(mac_abi::files::IconMove { cnid:            u32,
                                        expected_parent: u32,
                                        parent:          u32,
                                        placement:       u32,
                                        point:           mac_abi::geometry::Point });

wire_struct!(mac_abi::files::IconMoveCall {
    operation: u32,
    volume: u32,
    count: u32,
    changed: u32,
    when: u64,
    entries: *const mac_abi::files::IconMove,
});

wire_struct!(mac_abi::files::IconGroupCall {
    operation: u32,
    handle: u64,
    volume: u32,
    phase: u32,
    when: u64,
    count: u32,
    completed: u32,
    changed: u32,
    reserved: u32,
    entries: *const mac_abi::files::IconMove,
});

wire_struct!(mac_abi::files::FileTransferSource { volume:          u32,
                                                  cnid:            u32,
                                                  expected_parent: u32,
                                                  placement:       u32,
                                                  point:           mac_abi::geometry::Point,
                                                  reserved:        u32 });

wire_struct!(mac_abi::files::FileTransferCall {
    operation: u32,
    handle: u64,
    mode: u32,
    destination_volume: u32,
    destination_parent: u32,
    phase: u32,
    completed_items: u32,
    total_items: u32,
    completed_bytes: u64,
    total_bytes: u64,
    when: u64,
    error: i32,
    cleanup_error: i32,
    commit_decided: u32,
    sources: *const mac_abi::files::FileTransferSource,
    source_count: usize,
    current: mac_abi::files::CatalogInfo,
    reserved: [u32; 4],
});

wire_struct!(mac_abi::ResourceKey { kind: [u8; 4],
                                    id:   i16 });

wire_struct!(mac_abi::resources::ResourceInfo { key:         mac_abi::ResourceKey,
                                                origin:      mac_abi::resources::Scope,
                                                attributes:  u16,
                                                length:      usize,
                                                name_length: u32,
                                                name:        [u8; mac_abi::INLINE_STRING_BYTES] });

wire_struct!(mac_abi::resources::ResourceCall {
    operation: u32,
    scope: mac_abi::resources::Scope,
    reserved: u16,
    key: mac_abi::ResourceKey,
    index: u32,
    name: *const u8,
    name_length: usize,
    info: mac_abi::resources::ResourceInfo,
    output: *mut u8,
    capacity: usize,
    offset: usize,
    transferred: usize,
});

wire_struct!(mac_abi::resources::FileResourceCall {
    operation: u32,
    handle: u32,
    key: mac_abi::ResourceKey,
    index: u32,
    info: mac_abi::resources::ResourceInfo,
    has_name: u32,
    name: *const u8,
    name_length: usize,
    output: *mut u8,
    capacity: usize,
    offset: usize,
    transferred: usize,
    reserved: [u64; 2],
});

wire_struct!(mac_abi::resources::VersionInfo { number:       [u8; 4],
                                               country:      u16,
                                               reserved:     u16,
                                               short_length: u32,
                                               long_length:  u32,
                                               short:        [u8; mac_abi::INLINE_STRING_BYTES],
                                               long:         [u8; mac_abi::INLINE_STRING_BYTES] });

wire_struct!(mac_abi::resources::FileVersionCall { operation: u32,
                                                   volume:    u32,
                                                   cnid:      u32,
                                                   id:        i16,
                                                   reserved:  u16,
                                                   info:      mac_abi::resources::VersionInfo });

wire_struct!(mac_abi::textedit::EditorInfo { dest:             mac_abi::geometry::Rect,
                                             view:             mac_abi::geometry::Rect,
                                             selection_start:  u32,
                                             selection_end:    u32,
                                             text_length:      u32,
                                             line_count:       u32,
                                             line_height:      i16,
                                             ascent:           i16,
                                             active:           u16,
                                             caret_visible:    u16,
                                             revision:         u32,
                                             content_revision: u64,
                                             scroll_y:         u32,
                                             scroll_max:       u32 });

wire_struct!(mac_abi::textedit::LineInfo { start:    u32,
                                           end:      u32,
                                           width:    i16,
                                           reserved: u16 });

wire_struct!(mac_abi::textedit::TextEditCall {
    operation: u32,
    id: mac_abi::textedit::EditorId,
    window: mac_abi::WindowId,
    info: mac_abi::textedit::EditorInfo,
    line: mac_abi::textedit::LineInfo,
    index: u32,
    point: mac_abi::geometry::Point,
    delta: mac_abi::geometry::Point,
    key: u32,
    ticks: u32,
    input: *const u8,
    input_length: usize,
    output: *mut u8,
    capacity: usize,
    transferred: usize,
});

wire_struct!(mac_abi::textedit::TextEditFontCall { operation: u32,
                                                   id:        mac_abi::textedit::EditorId,
                                                   font:      mac_abi::quickdraw::FontSelection,
                                                   info:      mac_abi::textedit::EditorInfo });

wire_struct!(mac_abi::textedit::DisplayOptions { version:      u32,
                                                 wrap:         u32,
                                                 line_numbers: u32,
                                                 ruler_kind:   u32,
                                                 ruler_value:  i32 });

wire_struct!(mac_abi::textedit::DisplayGeometry { text_view:     mac_abi::geometry::Rect,
                                                  scroll_x:      u32,
                                                  scroll_max_x:  u32,
                                                  logical_lines: u32,
                                                  gutter_width:  u32,
                                                  ruler_x:       i32,
                                                  column_width:  u32,
                                                  monospaced:    u32,
                                                  features:      u32 });

wire_struct!(mac_abi::textedit::TextEditDisplayCall { operation: u32,
                                                      id:        mac_abi::textedit::EditorId,
                                                      options:   mac_abi::textedit::DisplayOptions,
                                                      x:         u32,
                                                      y:         u32,
                                                      info:      mac_abi::textedit::EditorInfo,
                                                      geometry:  mac_abi::textedit::DisplayGeometry });

wire_struct!(mac_abi::images::ImageInfo { width:      u32,
                                          height:     u32,
                                          bit_depth:  u8,
                                          color_type: u8,
                                          flags:      u16 });

wire_struct!(mac_abi::images::ImageCall {
    operation: u32,
    target: mac_abi::quickdraw::Target,
    data: *const u8,
    length: usize,
    surface: *mut mac_abi::Surface,
    destination: mac_abi::geometry::Rect,
    info: mac_abi::images::ImageInfo,
    error_kind: u32,
});

wire_struct!(mac_abi::controls::reflow::Cell { row:    u32,
                                               column: u32 });

wire_struct!(mac_abi::controls::reflow::Span { rows:    u32,
                                               columns: u32 });

wire_struct!(mac_abi::controls::reflow::Relation { parent:   u64,
                                                   revision: u64,
                                                   child:    u64,
                                                   cell:     mac_abi::controls::reflow::Cell,
                                                   span:     mac_abi::controls::reflow::Span });

wire_struct!(mac_abi::controls::reflow::AxisConstraint { kind:     u32,
                                                         extent:   i16,
                                                         reserved: u16 });

wire_struct!(mac_abi::controls::reflow::Constraints { width:
                                                          mac_abi::controls::reflow::AxisConstraint,
                                                      height:
                                                          mac_abi::controls::reflow::AxisConstraint });

wire_struct!(mac_abi::geometry::Dimensions { width:  i16,
                                             height: i16 });

wire_struct!(mac_abi::controls::reflow::OptionalDimensions { present: u32,
                                                             value:   mac_abi::geometry::Dimensions });

wire_struct!(mac_abi::controls::reflow::SizePreferences {
    minimum: mac_abi::controls::reflow::OptionalDimensions,
    preferred: mac_abi::controls::reflow::OptionalDimensions,
});

wire_struct!(mac_abi::controls::reflow::Requirements { minimum:   mac_abi::geometry::Dimensions,
                                                       preferred: mac_abi::geometry::Dimensions });

wire_struct!(mac_abi::controls::reflow::Measurement { intrinsic:
                                                          mac_abi::controls::reflow::Requirements,
                                                      constrained:
                                                          mac_abi::controls::reflow::Requirements });

wire_struct!(mac_abi::controls::reflow::SizeUnit { kind:  u32,
                                                   value: u32 });

wire_struct!(mac_abi::controls::reflow::SizeBounds { min:      mac_abi::controls::reflow::SizeUnit,
                                                     max:      mac_abi::controls::reflow::SizeUnit,
                                                     fill:     u32,
                                                     reserved: u32 });

wire_struct!(mac_abi::controls::reflow::TrackSetting { axis:   u32,
                                                       index:  u32,
                                                       bounds:
                                                           mac_abi::controls::reflow::SizeBounds });

wire_struct!(mac_abi::controls::reflow::BorderOffsets { left:   i16,
                                                        right:  i16,
                                                        top:    i16,
                                                        bottom: i16 });

wire_struct!(mac_abi::controls::reflow::LogicalRect { left:   i64,
                                                      top:    i64,
                                                      right:  i64,
                                                      bottom: i64 });

wire_struct!(mac_abi::controls::reflow::Scroll { axes:     u32,
                                                 bars:     u32,
                                                 x:        i64,
                                                 y:        i64,
                                                 width:    i64,
                                                 height:   i64,
                                                 viewport: mac_abi::geometry::Rect,
                                                 logical:  mac_abi::controls::reflow::LogicalRect });

wire_struct!(mac_abi::controls::reflow::SplitterSetting { axis:     u32,
                                                          index:    u32,
                                                          enabled:  u32,
                                                          reserved: u32 });

wire_struct!(mac_abi::controls::reflow::Payload { message:       i16,
                                                  reserved:      u16,
                                                  invalidation:  u32,
                                                  status:        i32,
                                                  constraints:
                                                      mac_abi::controls::reflow::Constraints,
                                                  rect:          mac_abi::geometry::Rect,
                                                  size:          mac_abi::geometry::Dimensions,
                                                  preferences:
                                                      mac_abi::controls::reflow::SizePreferences,
                                                  measurement:
                                                      mac_abi::controls::reflow::Measurement,
                                                  creation:      mac_abi::geometry::Dimensions,
                                                  child:         u64,
                                                  parent:        u64,
                                                  cell:          mac_abi::controls::reflow::Cell,
                                                  span:          mac_abi::controls::reflow::Span,
                                                  anchors:       u32,
                                                  track:
                                                      mac_abi::controls::reflow::TrackSetting,
                                                  window_border:
                                                      mac_abi::controls::reflow::BorderOffsets,
                                                  scroll:        mac_abi::controls::reflow::Scroll,
                                                  splitter:
                                                      mac_abi::controls::reflow::SplitterSetting });

wire_struct!(mac_abi::controls::ScrollbarState { bounds:      mac_abi::geometry::Rect,
                                                 value:       i16,
                                                 maximum:     i16,
                                                 active:      u32,
                                                 highlighted: i16,
                                                 reserved:    i16 });

wire_struct!(mac_abi::controls::ProgressRange { completed:   u64,
                                                in_progress: u64,
                                                total:       u64,
                                                determinate: u32,
                                                reserved:    u32 });

wire_struct!(mac_abi::definitions::ControlDefinitionState { bounds:
                                                                mac_abi::geometry::Rect,
                                                            title:
                                                                [u8; mac_abi::INLINE_STRING_BYTES],
                                                            title_length:    u32,
                                                            enabled:         u32,
                                                            highlighted:     u32,
                                                            value:           i16,
                                                            minimum:         i16,
                                                            maximum:         i16,
                                                            flags:           i16,
                                                            font:
                                                                mac_abi::quickdraw::FontSelection,
                                                            progress:
                                                                mac_abi::controls::ProgressRange,
                                                            splitter_parent:
                                                                mac_abi::geometry::Rect });

wire_struct!(mac_abi::controls::ControlCall {
    id: u64,
    relation: mac_abi::controls::reflow::Relation,
    reflow: u32,
    layout: mac_abi::controls::reflow::Payload,
    flags: u32,
    ticks: u64,
    step: i16,
    page: i16,
    operation: u32,
    window: u64,
    scrollbar: mac_abi::controls::ScrollbarState,
    point: mac_abi::geometry::Point,
    anchor: mac_abi::geometry::Point,
    result: i32,
    standard: mac_abi::definitions::ControlDefinitionState,
    procedure: i16,
    reserved: i16,
    surface: *mut mac_abi::Surface,
});

wire_struct!(mac_abi::scrollbox::Spec { viewport:            mac_abi::geometry::Rect,
                                        scrollbar:           mac_abi::geometry::Rect,
                                        font:                mac_abi::quickdraw::FontSelection,
                                        padding:             mac_abi::geometry::Point,
                                        row_height:          i16,
                                        reserved:            u16,
                                        count:               u32,
                                        foreground:          u32,
                                        background:          u32,
                                        selected_foreground: u32,
                                        selected_background: u32 });

wire_struct!(mac_abi::scrollbox::Call {
    operation: u32,
    id: u64,
    window: u64,
    flags: u32,
    spec: mac_abi::scrollbox::Spec,
    top: u32,
    first: u32,
    end: u32,
    row: u32,
    point: mac_abi::geometry::Point,
    ticks: u64,
    part: i16,
    reserved: [u16; 3],
    text_length: u32,
    text: [u8; mac_abi::scrollbox::TEXT_BYTES],
    surface: *mut mac_abi::Surface,
});

wire_struct!(mac_abi::memory::MemoryCall {
    operation: u32,
    handle: u32,
    other: u32,
    size: u32,
    offset: u32,
    state: u32,
    input: *const u8,
    input_length: usize,
    output: *mut u8,
    capacity: usize,
    length: usize,
    error: i32,
});

wire_struct!(mac_abi::scrap::ScrapInfo { size:    u32,
                                         count:   u16,
                                         flavors: u16 });

wire_struct!(mac_abi::scrap::FlavorInfo { kind:   [u8; 4],
                                          length: u32,
                                          offset: u32 });

wire_struct!(mac_abi::scrap::ScrapCall {
    operation: u32,
    kind: [u8; 4],
    index: u32,
    offset: u32,
    input: *const u8,
    input_length: usize,
    output: *mut u8,
    capacity: usize,
    transferred: usize,
    info: mac_abi::scrap::ScrapInfo,
    flavor: mac_abi::scrap::FlavorInfo,
});

wire_struct!(mac_abi::drag::Info { phase:     u32,
                                   outcome:   u32,
                                   items:     u32,
                                   receiver:  u32,
                                   window:    u64,
                                   point:     mac_abi::geometry::Point,
                                   modifiers: u16,
                                   action:    u16,
                                   error:     i32 });

wire_struct!(mac_abi::drag::Call {
    operation: u32,
    id: u32,
    receiver: u32,
    window: u64,
    item: u32,
    index: u32,
    volume: u32,
    parent: u32,
    kind: [u8; 4],
    flags: u32,
    error: i32,
    point: mac_abi::geometry::Point,
    bounds: mac_abi::geometry::Rect,
    info: mac_abi::drag::Info,
    input: *const u8,
    length: usize,
    output: *mut u8,
    capacity: usize,
    transferred: usize,
});

wire_struct!(mac_abi::events::EventCall { operation:    u32,
                                          mask:         u16,
                                          stop_mask:    u16,
                                          event:        mac_abi::events::Event,
                                          present:      u32,
                                          queued:       u32,
                                          keys:         mac_abi::events::KeyMap,
                                          ticks:        u32,
                                          uptime_ticks: u64 });

wire_struct!(mac_abi::events::EventPosition { window: u64,
                                              h:      i32,
                                              v:      i32 });

wire_struct!(mac_abi::events::PositionedEventCall { operation:     u32,
                                                    mask:          u16,
                                                    timeout_ticks: u32,
                                                    window:        u64,
                                                    event:         mac_abi::events::Event,
                                                    position:      mac_abi::events::EventPosition,
                                                    present:       u32,
                                                    waited:        u32 });

wire_struct!(mac_abi::events::MonotonicCall { nanoseconds: u64 });

wire_struct!(mac_abi::dates::ClockCall { unix_seconds: i64,
                                         microseconds: u32,
                                         reserved:     u32 });

wire_struct!(mac_abi::standard_file::StandardFileCall { operation:     u32,
                                                        handle:        u64,
                                                        mode:          u32,
                                                        volume:        u32,
                                                        directory:     u32,
                                                        type_count:    i32,
                                                        types:
                                                            [[u8; 4];
                                                             mac_abi::standard_file::MAX_TYPES],
                                                        prompt:        [u8; 768],
                                                        prompt_length: u32,
                                                        name:          [u8; 768],
                                                        name_length:   u32,
                                                        state:         u32,
                                                        replacing:     u32,
                                                        entry:         mac_abi::files::CatalogInfo,
                                                        reserved:      [u64; 2] });

wire_struct!(mac_abi::dialogs::DialogCall { operation:   u32,
                                            scope:       mac_abi::resources::Scope,
                                            resource:    i16,
                                            window:      u64,
                                            event:       mac_abi::events::Event,
                                            item:        u16,
                                            reserved:    u16,
                                            text_length: u32,
                                            text:        [u8; mac_abi::INLINE_STRING_BYTES] });

wire_struct!(mac_abi::menus::MenuIcon { scope:   mac_abi::resources::Scope,
                                        id:      i16,
                                        flags:   u16,
                                        present: u16 });

wire_struct!(mac_abi::menus::MenuItemState { text:        [u8; mac_abi::INLINE_STRING_BYTES],
                                             text_length: u32,
                                             bounds:      mac_abi::geometry::Rect,
                                             command:     u32,
                                             enabled:     u32,
                                             checked:     u32,
                                             key:         u8,
                                             mark:        u8,
                                             style:       u8,
                                             separator:   u8,
                                             icon:        mac_abi::menus::MenuIcon });

// Genuine direct trusted MDEF calls use this checked user-memory DTO.
wire_struct!(mac_abi::menus::MenuDefinitionState { minimum_width: i16,
                                                   profile:       u16,
                                                   menu_id:       i16,
                                                   highlighted:   i16,
                                                   enabled:       u32,
                                                   bar_selected:  u32,
                                                   first:         u32,
                                                   title:         [u8;
                                                                   mac_abi::INLINE_STRING_BYTES],
                                                   title_length:  u32,
                                                   title_icon:    mac_abi::menus::MenuIcon,
                                                   bar_bounds:    mac_abi::geometry::Rect,
                                                   title_bounds:  mac_abi::geometry::Rect,
                                                   popup_bounds:  mac_abi::geometry::Rect,
                                                   item_count:    u32,
                                                   items:         [mac_abi::menus::MenuItemState;
                                                                   mac_abi::menus::MAX_ITEMS] });
wire_struct!(mac_abi::menus::MenuCall { operation:    u32,
                                        handle:       u64,
                                        scope:        mac_abi::resources::Scope,
                                        menu_id:      i16,
                                        item:         u16,
                                        reserved:     u16,
                                        value:        u32,
                                        title_bounds: mac_abi::geometry::Rect,
                                        popup_bounds: mac_abi::geometry::Rect,
                                        command:      u32,
                                        replacement:  mac_abi::menus::MenuItemState });

wire_struct!(mac_abi::processes::ProcessInfo { id:          u64,
                                               thread:      u64,
                                               image:       u64,
                                               volume:      u32,
                                               cnid:        u32,
                                               foreground:  u32,
                                               hidden:      u32,
                                               name:        [u8; mac_abi::INLINE_STRING_BYTES],
                                               name_length: u32,
                                               switches:    u64 });

wire_struct!(mac_abi::processes::ProcessCall { operation:           u32,
                                               index:               u32,
                                               id:                  u64,
                                               volume:              u32,
                                               cnid:                u32,
                                               info:                mac_abi::processes::ProcessInfo,
                                               requirement_class:   [u8; 4],
                                               requirement_version: u32 });

wire_struct!(mac_abi::processes::DocumentCall { drag:                u32,
                                                operation:           u32,
                                                handle:              u64,
                                                application_volume:  u32,
                                                application_cnid:    u32,
                                                volume:              u32,
                                                cnid:                u32,
                                                file_type:           [u8; 4],
                                                index:               u32,
                                                count:               u32,
                                                present:             u32,
                                                reserved:            [u32; 2],
                                                entry:               mac_abi::files::CatalogInfo,
                                                process:
                                                    mac_abi::processes::ProcessInfo,
                                                requirement_class:   [u8; 4],
                                                requirement_version: u32 });

wire_struct!(mac_abi::processes::NativeExitCall { code:     i32,
                                                  flags:    u32,
                                                  reserved: u64 });

wire_struct!(mac_abi::application_heap::Call {
    operation: u32,
    reserved: u32,
    pointer: *mut u8,
    old_size: usize,
    alignment: usize,
    new_size: usize,
    result: *mut u8,
    detail_length: u32,
    reserved2: u32,
    detail: [u8; mac_abi::application_heap::DETAIL_BYTES],
});

wire_struct!(mac_abi::native_threads::Call {
    operation: u32,
    flags: u32,
    reserved: [u64; 2],
    entry: usize,
    data: usize,
    stack_bytes: u64,
    thread: u64,
    right: u64,
    seconds: u64,
    nanoseconds: u32,
    name_length: u32,
    name: *const u8,
    state: u32,
    outcome: u32,
    parallelism: u32,
    reserved2: u32,
    detail_length: u32,
    reserved3: u32,
    detail: [u8; mac_abi::native_threads::DETAIL_BYTES],
});

wire_struct!(mac_abi::address_wait::Call { operation:     u32,
                                           flags:         u32,
                                           address:       usize,
                                           expected:      u32,
                                           nanoseconds:   u32,
                                           seconds:       u64,
                                           reserved:      [u64; 2],
                                           outcome:       u32,
                                           awakened:      u32,
                                           detail_length: u32,
                                           reserved2:     u32,
                                           detail:        [u8; mac_abi::address_wait::DETAIL_BYTES] });

wire_struct!(mac_abi::tls::Call { operation:     u32,
                                  flags:         u32,
                                  reserved:      [u64; 2],
                                  key:           u64,
                                  value:         usize,
                                  destructor:    usize,
                                  ticket:        u64,
                                  present:       u32,
                                  round:         u32,
                                  detail_length: u32,
                                  reserved2:     u32,
                                  detail:        [u8; mac_abi::tls::DETAIL_BYTES] });

wire_struct!(mac_abi::entropy::Call {
    operation: u32,
    flags: u32,
    reserved: [u64; 2],
    output: *mut u8,
    capacity: u32,
    written: u32,
    detail_length: u32,
    reserved2: u32,
    detail: [u8; mac_abi::entropy::DETAIL_BYTES],
});

wire_struct!(mac_abi::pipes::Call {
    operation: u32,
    nonblocking: u32,
    handle: u64,
    capacity: u32,
    name_length: u32,
    buffer: *mut u8,
    length: usize,
    transferred: usize,
    reader: u64,
    writer: u64,
    name: [u8; mac_abi::pipes::NAME_BYTES],
    reserved: [u64; 2],
    writers: u64,
});

wire_struct!(mac_abi::power::PowerCall { operation:     u32,
                                         reserved:      u32,
                                         capabilities:  u64,
                                         failure_stage: u32 });

wire_struct!(mac_abi::crashes::CrashInfo { window:       mac_abi::WindowId,
                                           report_bytes: usize,
                                           line_count:   u32,
                                           first_line:   u32,
                                           page_lines:   u32,
                                           dismissed:    u32,
                                           flags:        u32,
                                           app:          u64,
                                           thread:       u64 });

wire_struct!(mac_abi::crashes::CrashCall {
    operation: u32,
    flags: u32,
    data: *const u8,
    length: usize,
    offset: usize,
    output: *mut u8,
    capacity: usize,
    delta: i32,
    info: mac_abi::crashes::CrashInfo,
});

wire_struct!(mac_abi::inspection::Pool { registration:        u64,
                                         kind:                [u8; 4],
                                         policy:              u32,
                                         limit:               u64,
                                         live:                u64,
                                         retired:             u64,
                                         pending:             u64,
                                         quarantined:         u64,
                                         completion_faults:   u64,
                                         scoped_children:     u64,
                                         slots:               u64,
                                         births:              u64,
                                         deaths:              u64,
                                         generation_headroom: u64,
                                         reason:              [u8;
                                                               mac_abi::inspection::DETAIL_BYTES],
                                         reason_length:       u32,
                                         flags:               u32 });

wire_struct!(mac_abi::inspection::RegisteredKind { id:              u64,
                                                   owner:           u64,
                                                   owner_kind:      u32,
                                                   namespace:       [u8; 4],
                                                   policy:          u32,
                                                   admitting:       u32,
                                                   limit:           u64,
                                                   uses:            u64,
                                                   label:           [u8;
                                                                     mac_abi::INLINE_STRING_BYTES],
                                                   label_length:    u32,
                                                   provider:        [u8;
                                                                     mac_abi::INLINE_STRING_BYTES],
                                                   provider_length: u32,
                                                   source:          u32,
                                                   bundle:          [u8; 16],
                                                   resource_kind:   [u8; 4],
                                                   resource_id:     i16,
                                                   reserved:        u16,
                                                   reason:
                                                       [u8; mac_abi::inspection::DETAIL_BYTES],
                                                   reason_length:   u32 });

wire_struct!(mac_abi::inspection::AccountUsage { limit:     u64,
                                                 reserved:  u64,
                                                 committed: u64 });

wire_struct!(mac_abi::inspection::Account { coverage:    u32,
                                            closing:     u32,
                                            kernel:      mac_abi::inspection::AccountUsage,
                                            application: mac_abi::inspection::AccountUsage });

wire_struct!(mac_abi::inspection::Domain { incarnation:       u64,
                                           kind:              u32,
                                           flags:             u32,
                                           ledger_revision:   u64,
                                           kinds:             u64,
                                           live:              u64,
                                           retired:           u64,
                                           pending:           u64,
                                           quarantined:       u64,
                                           completion_faults: u64,
                                           scoped_children:   u64,
                                           slots:             u64,
                                           births:            u64,
                                           deaths:            u64,
                                           name:              [u8; mac_abi::INLINE_STRING_BYTES],
                                           name_length:       u32,
                                           reserved:          u32,
                                           account:           mac_abi::inspection::Account });

wire_struct!(mac_abi::inspection::CompletionFault { owner:         u64,
                                                    kind:          [u8; 4],
                                                    cause:         u32,
                                                    slot:          u64,
                                                    generation:    u64,
                                                    registration:  u64,
                                                    detail:
                                                        [u8;
                                                         mac_abi::inspection::FAULT_DETAIL_BYTES],
                                                    detail_length: u32,
                                                    flags:         u32 });

wire_struct!(mac_abi::inspection::Call { operation:       u32,
                                         scope:           u32,
                                         revision:        u64,
                                         module_revision: u64,
                                         index:           u64,
                                         count:           u64,
                                         coverage:        u64,
                                         ledger_revision: u64,
                                         subject:         u64,
                                         pool_index:      u64,
                                         pool:            mac_abi::inspection::Pool,
                                         kind:            mac_abi::inspection::RegisteredKind,
                                         domain:          mac_abi::inspection::Domain,
                                         fault:           mac_abi::inspection::CompletionFault,
                                         detail:          [u8; mac_abi::inspection::DETAIL_BYTES],
                                         detail_length:   u32,
                                         reserved:        u32 });

wire_struct!(mac_abi::inspection::memory::Summary { revision:                               u64,
                                                    page_bytes:                             u64,
                                                    ram_bytes:                              u64,
                                                    free_pages:                             u64,
                                                    heap_used:                              u64,
                                                    heap_capacity:                          u64,
                                                    root_physical:                          u64,
                                                    region_count:                           u64,
                                                    ordinary_foundation_heap_bytes:         u64,
                                                    tracked_foundation_physical_bytes:      u64,
                                                    tracked_ordinary_system_physical_bytes: u64,
                                                    flags:                                  u32,
                                                    reserved:                               u32 });

wire_struct!(mac_abi::inspection::memory::Region { virtual_base:  u64,
                                                   physical_base: u64,
                                                   bytes:         u64,
                                                   protection:    u32,
                                                   kind:          u32 });

wire_struct!(mac_abi::inspection::memory::Call { operation:     u32,
                                                 source:        u32,
                                                 revision:      u64,
                                                 index:         u64,
                                                 next:          u64,
                                                 count:         u32,
                                                 flags:         u32,
                                                 summary:
                                                     mac_abi::inspection::memory::Summary,
                                                 regions:
                                                     [mac_abi::inspection::memory::Region;
                                                      mac_abi::inspection::memory::BATCH],
                                                 detail:        [u8;
                                                                 mac_abi::inspection::DETAIL_BYTES],
                                                 detail_length: u32,
                                                 reserved:      u32 });

wire_struct!(mac_abi::inspection::cpus::Summary { count:            u64,
                                                  ticks_per_second: u64 });

wire_struct!(mac_abi::inspection::cpus::Row { hart:            u64,
                                              idle_ticks:      u64,
                                              executing_ticks: u64,
                                              sampled_at:      u64,
                                              state:           u32,
                                              flags:           u32,
                                              activity:        u32,
                                              reserved:        u32 });

wire_struct!(mac_abi::inspection::cpus::Call { operation:     u32,
                                               reserved:      u32,
                                               index:         u64,
                                               summary:       mac_abi::inspection::cpus::Summary,
                                               row:           mac_abi::inspection::cpus::Row,
                                               detail:        [u8;
                                                               mac_abi::inspection::DETAIL_BYTES],
                                               detail_length: u32,
                                               reserved_tail: u32 });

wire_struct!(mac_abi::theme::ScrollbarColors { ink:          u32,
                                               accent_dark:  u32,
                                               gray_dark:    u32,
                                               accent_mid:   u32,
                                               gray_mid:     u32,
                                               accent_light: u32,
                                               face:         u32,
                                               accent_pale:  u32,
                                               gray_light:   u32,
                                               face_light:   u32,
                                               highlight:    u32 });

wire_struct!(mac_abi::theme::Palette { revision:        u64,
                                       text:            u32,
                                       content:         u32,
                                       selected_text:   u32,
                                       selection:       u32,
                                       secondary_text:  u32,
                                       disabled_text:   u32,
                                       control_outline: u32,
                                       progress_fill:   u32,
                                       progress_track:  u32,
                                       scrollbar:       mac_abi::theme::ScrollbarColors,
                                       separator:       u32 });

wire_struct!(mac_abi::definitions::CallbackCall {
    header: mac_abi::Header,
    operation: u32,
    reserved: u32,
    buffer: *mut u8,
    length: usize,
    required: usize,
    present: u32,
    reserved2: u32,
    result: i64,
});
wire_struct!(mac_abi::definitions::UserCall { header:    mac_abi::Header,
                                              kind:      u32,
                                              id:        i16,
                                              variation: i16,
                                              message:   i16,
                                              reserved:  u16,
                                              reflow:    u32,
                                              param:     i64,
                                              object:    usize,
                                              surface:   usize,
                                              result:    i64 });
wire_struct!(mac_abi::definitions::DefinitionPanic { length:  u32,
                                                     flags:   u32,
                                                     message: [u8; 1024] });

wire_struct!(mac_abi::Header { version: u32,
                               size:    u32 });
