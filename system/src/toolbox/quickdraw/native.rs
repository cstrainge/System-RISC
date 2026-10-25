//! Native QuickDraw ABI boundary. Algorithms consume checked Rust slices below it.
use mac_abi::{quickdraw::*, traps::*, *};

use super::{SurfaceExt, fonts::FontManager, fonts_service, regions_service, service};
unsafe fn request<'a, T>(pointer: *mut (), size: usize) -> Result<&'a mut T, Status>
{
    if pointer.is_null()
       || size != core::mem::size_of::<T>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<T>())
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(unsafe { &mut *pointer.cast::<T>() })
}
unsafe fn input<'a, T>(pointer: *const T, length: usize, maximum: usize)
                       -> Result<&'a [T], Status>
{
    if length == 0
    {
        return Ok(&[]);
    }
    if pointer.is_null()
       || length > maximum
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<T>())
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(unsafe { core::slice::from_raw_parts(pointer, length) })
}
unsafe fn text<'a>(pointer: *const u8, length: usize) -> Result<&'a str, Status>
{
    core::str::from_utf8(unsafe { input(pointer, length, 65535) }?).map_err(|_| BAD_ARGUMENT)
}
/// # Safety
/// The call's data pointer must borrow readable bytes of the declared length.
pub(crate) unsafe fn icon_bytes(call: &IconDataCall)
                                -> Result<super::icons_service::Drawing, Status>
{
    if call.reserved != [0; 2]
    {
        return Err(BAD_ARGUMENT);
    }
    let data = unsafe { input(call.data, call.length, 256 * 1024) }?;
    super::icons_service::Drawing::bytes(call.kind, data, call.destination)
}
/// # Safety
/// Payload and all nested pointers borrow valid storage for their declared sizes;
/// output/surface memory is exclusive and does not alias input or the payload.
pub unsafe fn dispatch(number: u32, pointer: *mut (), size: usize, fonts: Option<&FontManager>)
                       -> i64
{
    let result = (|| -> Result<(), Status> {
        match number
        {
            IMAGE =>
            {
                use mac_abi::images::*;
                let call = unsafe { request::<ImageCall>(pointer, size) }?;
                call.error_kind = 0;
                call.info = ImageInfo::default();
                let result = (|| -> mac_abi::images::Result<()> {
                    call.target.decode().map_err(|_| {
                                             ImageError { status: BAD_ARGUMENT,
                                                          kind:   INVALID_DESTINATION }
                                         })?;
                    if !call.surface.is_null() && call.target != Target::default()
                    {
                        return Err(ImageError { status: BAD_ARGUMENT,
                                                kind:   INVALID_DESTINATION });
                    }

                    if call.operation > mac_abi::images::DRAW
                    {
                        return Err(ImageError { status: NOT_SUPPORTED,
                                                kind:   UNSUPPORTED });
                    }
                    if call.length > MAX_INPUT_BYTES
                    {
                        return Err(ImageError { status: NO_MEMORY,
                                                kind:   LIMIT_EXCEEDED });
                    }
                    let data = unsafe { input(call.data, call.length, MAX_INPUT_BYTES) }
                        .map_err(|status| ImageError { status, kind: TRUNCATED })?;
                    if call.operation == INFO
                    {
                        call.info = super::png::info(data)?;
                        return Ok(());
                    }
                    let drawing =
                        super::png::Drawing::new(data, call.destination, service::Port::default())?;
                    call.info = drawing.image.info;
                    if call.surface.is_null()
                    {
                        #[cfg(target_arch = "riscv64")]
                        return crate::kernel::with_desktop(|desktop| desktop.image(call, drawing))
                            .unwrap_or_else(|error|Err(error.status()))
                            .map_err(|status| ImageError { status, kind: SERVICE_ERROR });
                        #[cfg(not(target_arch = "riscv64"))]
                        return Err(ImageError { status: NOT_SUPPORTED,
                                                kind:   SERVICE_ERROR });
                    }
                    let surface = unsafe {
                                      request::<Surface>(call.surface.cast(),
                                                         core::mem::size_of::<Surface>())
                                  }.map_err(|status| {
                                       ImageError { status,
                                                    kind: INVALID_DESTINATION }
                                   })?;
                    let mut canvas =
                        unsafe { surface.canvas() }.ok_or(ImageError { status: BAD_ARGUMENT,
                                            kind:   INVALID_DESTINATION })?;
                    drawing.paint(&mut canvas);
                    Ok(())
                })();
                result.map_err(|error| {
                          call.error_kind = error.kind;
                          error.status
                      })
            }
            QUICKDRAW =>
            {
                let call = unsafe { request::<GraphicsCall>(pointer, size) }?;
                call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                if !call.surface.is_null() && call.target != Target::default()
                {
                    return Err(BAD_ARGUMENT);
                }
                let points = unsafe { input(call.points, call.point_count, 4096) }?;
                let region = unsafe { input(call.region, call.region_length, 65535) }?;
                if call.surface.is_null()
                {
                    #[cfg(target_arch="riscv64")]
                    return crate::kernel::with_desktop(|d|d.graphics(call,points,region)).unwrap_or_else(|error|Err(error.status()));
                    #[cfg(not(target_arch = "riscv64"))]
                    return Err(NOT_SUPPORTED);
                }
                let surface = unsafe {
                    request::<Surface>(call.surface.cast(), core::mem::size_of::<Surface>())
                }?;
                let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                let mut port = service::Port::from_wire(call.state)?;
                if let Some(drawing) = service::graphics(&mut port, call, points, region)?
                {
                    drawing.paint(&mut canvas)?;
                }
                Ok(())
            }
            FONT =>
            {
                let call = unsafe { request::<FontCall>(pointer, size) }?;
                call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                if !call.surface.is_null() && call.target != Target::default()
                {
                    return Err(BAD_ARGUMENT);
                }
                let manager = fonts.ok_or(NOT_SUPPORTED)?;
                let text = unsafe { text(call.text, call.text_length) }?;
                let status = if call.operation == FONT_DRAW
                {
                    if call.surface.is_null()
                    {
                        #[cfg(target_arch="riscv64")]
                        return crate::kernel::with_desktop(|d|d.font_draw(call,text,manager)).unwrap_or_else(|error|Err(error.status()));
                        #[cfg(not(target_arch = "riscv64"))]
                        return Err(NOT_SUPPORTED);
                    }
                    let surface = unsafe {
                        request::<Surface>(call.surface.cast(), core::mem::size_of::<Surface>())
                    }?;
                    let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                    fonts_service::draw(manager, call, text, &mut canvas)
                }
                else
                {
                    fonts_service::query(manager, call, text)
                };
                if status == OK { Ok(()) } else { Err(status) }
            }
            REGION =>
            {
                let call = unsafe { request::<RegionCall>(pointer, size) }?;
                let a = unsafe { input(call.a, call.a_length, 65535) }?;
                let b = unsafe { input(call.b, call.b_length, 65535) }?;
                let points = unsafe { input(call.points, call.point_count, 4096) }?;
                if let Some(bytes) = regions_service::compute(call, a, b, points)?
                {
                    if call.capacity == 0
                    {
                        return Ok(());
                    }
                    if call.output.is_null() || call.capacity < bytes.len()
                    {
                        return Err(NO_MEMORY);
                    }
                    unsafe {
                        core::ptr::copy_nonoverlapping(bytes.as_ptr(), call.output, bytes.len());
                    }
                }
                Ok(())
            }
            COPY_BITS =>
            {
                let call = unsafe { request::<BitmapCall>(pointer, size) }?;
                call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                if !call.surface.is_null() && call.target != Target::default()
                {
                    return Err(BAD_ARGUMENT);
                }
                let data = unsafe { input(call.data, call.data_length, 2 * 1024 * 1024) }?;
                let palette = unsafe { input(call.palette, call.palette_length, 256) }?;
                let mask = unsafe { input(call.mask, call.mask_length, 65535) }?;
                if call.surface.is_null()
                {
                    #[cfg(target_arch="riscv64")]
                    return crate::kernel::with_desktop(|d|d.copy_bits(call,data,palette,mask)).unwrap_or_else(|error|Err(error.status()));
                    #[cfg(not(target_arch = "riscv64"))]
                    return Err(NOT_SUPPORTED);
                }
                let surface = unsafe {
                    request::<Surface>(call.surface.cast(), core::mem::size_of::<Surface>())
                }?;
                let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                super::bitmaps_service::BitmapDrawing::new(call,data,palette,mask,service::Port::default())?.paint(&mut canvas)
            }
            WORLD =>
            {
                let call = unsafe { request::<WorldCall>(pointer, size) }?;
                if call.pixel_length > crate::display::PIXELS
                {
                    return Err(BAD_ARGUMENT);
                }
                let pixels = if call.pixel_length == 0
                {
                    &mut [][..]
                }
                else
                {
                    if call.pixels.is_null() || !(call.pixels as usize).is_multiple_of(4)
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    unsafe { core::slice::from_raw_parts_mut(call.pixels, call.pixel_length) }
                };
                #[cfg(target_arch="riscv64")]
                return crate::kernel::with_desktop(|d|d.world(call,pixels)).unwrap_or_else(|error|Err(error.status()));
                #[cfg(not(target_arch = "riscv64"))]
                {
                    let _ = pixels;
                    Err(NOT_SUPPORTED)
                }
            }
            CURSOR =>
            {
                if size == core::mem::size_of::<CursorResourceCall>()
                {
                    let call = unsafe { request::<CursorResourceCall>(pointer, size) }?;
                    #[cfg(target_arch = "riscv64")]
                    return crate::kernel::with_desktop(|d| d.cursor_resource(call))
                        .unwrap_or_else(|error| Err(error.status()));
                    #[cfg(not(target_arch = "riscv64"))]
                    {
                        let _ = call;
                        return Err(NOT_SUPPORTED);
                    }
                }
                let call = unsafe { request::<CursorCall>(pointer, size) }?;
                let colors = unsafe { input(call.colors, call.color_count, 256) }?;
                #[cfg(target_arch="riscv64")]
                return crate::kernel::with_desktop(|d|d.cursor(call,colors)).unwrap_or_else(|error|Err(error.status()));
                #[cfg(not(target_arch = "riscv64"))]
                {
                    let _ = colors;
                    Err(NOT_SUPPORTED)
                }
            }
            MAPPING =>
            {
                let call = unsafe { request::<MappingCall>(pointer, size) }?;
                match call.operation
                {
                    MAP_POINT =>
                    {
                        call.point = super::geometry::map_point(call.point, call.source,
                                                                call.destination).map_err(|_| {
                                                                                     BAD_ARGUMENT
                                                                                 })?
                    }
                    SCALE_POINT =>
                    {
                        let sw = i64::from(call.source.right) - i64::from(call.source.left);
                        let sh = i64::from(call.source.bottom) - i64::from(call.source.top);
                        if sw == 0 || sh == 0
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        call.point =
                            Point { h: i16::try_from(i64::from(call.point.h)
                                                     * (i64::from(call.destination.right)
                                                        - i64::from(call.destination.left))
                                                     / sw).map_err(|_| BAD_ARGUMENT)?,
                                    v: i16::try_from(i64::from(call.point.v)
                                                     * (i64::from(call.destination.bottom)
                                                        - i64::from(call.destination.top))
                                                     / sh).map_err(|_| BAD_ARGUMENT)? };
                    }
                    MAP_RECTANGLE =>
                    {
                        call.rect = super::geometry::map_rect(call.rect, call.source,
                                                              call.destination).map_err(|_| {
                                                                                   BAD_ARGUMENT
                                                                               })?
                    }
                    MAP_POLYGON =>
                    {
                        let points = unsafe { input(call.points, call.point_count, 4096) }?;
                        let mapped=super::geometry::map_polygon(points,call.source,call.destination).map_err(|_|BAD_ARGUMENT)?;
                        if !mapped.is_empty()
                        {
                            unsafe {
                                core::ptr::copy_nonoverlapping(mapped.as_ptr(),
                                                               call.points,
                                                               mapped.len());
                            }
                        }
                    }
                    _ => return Err(NOT_SUPPORTED)
                }
                Ok(())
            }
            RESOURCE =>
            {
                #[cfg(target_arch = "riscv64")]
                if size == size_of::<mac_abi::resources::FileResourceCall>()
                {
                    let owner = crate::context::current_thread_context().identity().app.0;
                    let status = unsafe {
                        crate::toolbox::resources::inspect::dispatch_async(owner, pointer, size)
                    };
                    return if status == 0 { Ok(()) } else { Err(status as Status) };
                }
                #[cfg(target_arch = "riscv64")]
                if size == size_of::<mac_abi::resources::FileVersionCall>()
                {
                    let status = crate::kernel::with_files(|files| unsafe {
                                     crate::toolbox::resources::file::dispatch(files, pointer, size)
                                 }).unwrap_or_else(|error| error.status() as i64);
                    return if status == 0 { Ok(()) } else { Err(status as Status) };
                }
                #[cfg(target_arch = "riscv64")]
                return crate::kernel::resources().map(|manager| unsafe {
                           crate::toolbox::resources::native::dispatch(manager, pointer, size)
                       })
                       .map_or(Err(NOT_SUPPORTED), |status| {
                           if status == 0 { Ok(()) } else { Err(status as Status) }
                       });
                #[cfg(not(target_arch = "riscv64"))]
                return Err(NOT_SUPPORTED);
            }
            PICTURE =>
            {
                let call = unsafe { request::<PictureCall>(pointer, size) }?;
                call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                if !call.surface.is_null() && call.target != Target::default()
                {
                    return Err(BAD_ARGUMENT);
                }
                let fonts = fonts.ok_or(NOT_SUPPORTED)?;
                let bytes = unsafe { input(call.data, call.length, 2 * 1024 * 1024) }?;
                #[cfg(target_arch = "riscv64")]
                let bytes = if call.operation == PICTURE_RESOURCE
                {
                    crate::kernel::resources().ok_or(NOT_SUPPORTED)?
                                              .get(call.scope,
                                                   ResourceKey { kind: *b"PICT",
                                                                 id:   call.id })?
                                              .1
                                              .data
                                              .as_slice()
                }
                else
                {
                    bytes
                };
                if call.operation > PICTURE_INFO
                {
                    return Err(NOT_SUPPORTED);
                }
                let drawing = super::pictures_service::Drawing::new(bytes,
                                                                    call.destination,
                                                                    service::Port::default(),
                                                                    fonts)?;
                call.bounds = drawing.picture.bounds;
                call.command_count = drawing.picture.commands.len() as u32;
                if call.operation == PICTURE_INFO
                {
                    return Ok(());
                }
                if call.surface.is_null()
                {
                    #[cfg(target_arch="riscv64")]
                    return crate::kernel::with_desktop(|d|d.picture(call,drawing,fonts)).unwrap_or_else(|error|Err(error.status()));
                    #[cfg(not(target_arch = "riscv64"))]
                    return Err(NOT_SUPPORTED);
                }
                let surface = unsafe {
                    request::<Surface>(call.surface.cast(), core::mem::size_of::<Surface>())
                }?;
                let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                drawing.paint(&mut canvas, fonts)
            }
            ICON =>
            {
                if size == size_of::<IconDataCall>()
                {
                    let call = unsafe { request::<IconDataCall>(pointer, size) }?;
                    call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                    if !call.surface.is_null() && call.target != Target::default()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let drawing = unsafe { icon_bytes(call) }?;
                    if call.surface.is_null()
                    {
                        #[cfg(target_arch = "riscv64")]
                        return crate::kernel::with_desktop(|d| {
                                   d.icon(&IconCall { target: call.target,
                                                      ..Default::default() },
                                          drawing)
                               }).unwrap_or_else(|e| Err(e.status()));
                        #[cfg(not(target_arch = "riscv64"))]
                        return Err(NOT_SUPPORTED);
                    }
                    let surface =
                        unsafe { request::<Surface>(call.surface.cast(), size_of::<Surface>()) }?;
                    return drawing.paint(&mut unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?);
                }
                #[cfg(target_arch = "riscv64")]
                if size == size_of::<DefaultFileIconCall>()
                {
                    let call = unsafe { request::<DefaultFileIconCall>(pointer, size) }?;
                    call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                    if !call.surface.is_null() && call.target != Target::default()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if call.operation != ICON_PLOT_FILE_DEFAULT
                       || call.reserved != 0
                       || call.name_length > 255
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let name=core::str::from_utf8(&call.name[..call.name_length as usize]).map_err(|_|BAD_ARGUMENT)?;
                    if name.contains('\0')
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let drawing =
                        super::icons_service::Drawing::file_default(call.file_type, name,
                                                                    call.destination, call.flags)?;
                    if call.surface.is_null()
                    {
                        return crate::kernel::with_desktop(|d| {
                                   d.icon(&IconCall { target: call.target,
                                                      ..Default::default() },
                                          drawing)
                               }).unwrap_or_else(|error| Err(error.status()));
                    }
                    let surface =
                        unsafe { request::<Surface>(call.surface.cast(), size_of::<Surface>()) }?;
                    return drawing.paint(&mut unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?);
                }
                #[cfg(target_arch = "riscv64")]
                if size == size_of::<FileIconCall>()
                {
                    let call = unsafe { request::<FileIconCall>(pointer, size) }?;
                    call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                    if !call.surface.is_null() && call.target != Target::default()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let manager = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
                    if call.operation == ICON_LOAD_FILE
                    {
                        return crate::kernel::with_files(|files| {
                                   super::icons_service::load_file(files, manager, call)
                               }).map_err(|e| e.status())?;
                    }
                    let drawing = crate::kernel::with_files(|files| {
                                      super::icons_service::Drawing::file(files, manager, call)
                                  }).map_err(|e| e.status())??;
                    if call.surface.is_null()
                    {
                        let target = IconCall { target: call.target,
                                                ..Default::default() };
                        return crate::kernel::with_desktop(|d| d.icon(&target, drawing))
                            .unwrap_or_else(|error| Err(error.status()));
                    }
                    let surface =
                        unsafe { request::<Surface>(call.surface.cast(), size_of::<Surface>()) }?;
                    let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                    return drawing.paint(&mut canvas);
                }
                let call = unsafe { request::<IconCall>(pointer, size) }?;
                call.target.decode().map_err(|_| BAD_ARGUMENT)?;
                if !call.surface.is_null() && call.target != Target::default()
                {
                    return Err(BAD_ARGUMENT);
                }
                #[cfg(not(target_arch = "riscv64"))]
                {
                    let _ = call;
                    Err(NOT_SUPPORTED)
                }
                #[cfg(target_arch = "riscv64")]
                {
                    let manager = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
                    if call.operation != ICON_PLOT
                    {
                        return super::icons_service::query(manager, call);
                    }
                    let drawing = super::icons_service::Drawing::new(manager,
                                                                     call,
                                                                     service::Port::default())?;
                    if call.surface.is_null()
                    {
                        return crate::kernel::with_desktop(|d|d.icon(call,drawing)).unwrap_or_else(|error|Err(error.status()));
                    }
                    let surface = unsafe {
                        request::<Surface>(call.surface.cast(), core::mem::size_of::<Surface>())
                    }?;
                    let mut canvas = unsafe { surface.canvas() }.ok_or(BAD_ARGUMENT)?;
                    drawing.paint(&mut canvas)
                }
            }
            _ => Err(NOT_SUPPORTED)
        }
    })();
    match result
    {
        Ok(()) => OK as i64,
        Err(status) => status as i64
    }
}
