//! The native side uses the public Rust SDK; the legacy side deliberately calls
//! only the raw trap ABI, just as a translated classic application does.
use mac_abi::{controls::*, quickdraw::*, resources::*, traps::*, *};
use mac_toolbox::System;

use super::{Failure, require};
fn call<T>(gate: TrapGate, number: u32, request: &mut T) -> i64
{
    // Every caller supplies initialized live request/payload borrows.
    unsafe { gate(number, core::ptr::from_mut(request).cast(), size_of::<T>()) }
}
pub fn shared(system: &System<'_>, gate: TrapGate) -> Result<(), Failure>
{
    // Independent System 7 mapping sample from original byte-title round trip.
    let pascal = b"\x07Caf\x8e\xdb\xf0\xff";
    let native = "Café¤\u{f8ff}ˇ";
    let mut native_pixels = [0xffffff; 2048];
    let mut legacy_pixels = native_pixels;
    let mut surface = Surface { pixels: legacy_pixels.as_mut_ptr(),
                                width:  128,
                                height: 16,
                                origin: Point::default(),
                                clip:   Rect::new(0, 0, 128, 16) };
    let mut native_surface = Surface { pixels: native_pixels.as_mut_ptr(),
                                       width:  128,
                                       height: 16,
                                       origin: Point::default(),
                                       clip:   Rect::new(0, 0, 128, 16) };
    let connection = unsafe { mac_traps::SurfaceConnection::new(&mut native_surface, gate) };
    let mut port = mac_toolbox::quickdraw::SurfacePort::new(connection);
    let expected_width = port.text_width(0, native)?;
    let mut request = SurfaceCall { operation: 4,
                                    surface: &mut surface,
                                    text: pascal.as_ptr(),
                                    text_length: pascal.len(),
                                    ..Default::default() };
    require(call(gate, PSTRING_SURFACE, &mut request) == 0, "Pascal text width status")?;
    require(request.width == expected_width, "Pascal decoded width")?;
    port.draw_text(0, 2, 12, native, 128, 0)?;
    request.operation = 6;
    request.x = 2;
    request.y = 12;
    request.foreground = 0;
    request.limit = 128;
    require(call(gate, PSTRING_SURFACE, &mut request) == 0, "Pascal draw status")?;
    require(native_pixels == legacy_pixels, "Pascal draw UTF8 pixel equality")?;
    require(request.text == pascal.as_ptr() && request.text_length == pascal.len(),
            "Pascal borrowed pointer restored")?;
    for bytes in [&b""[..], &b"\x03ab"[..], &b"\0trailing"[..]]
    {
        request.text = bytes.as_ptr();
        request.text_length = bytes.len();
        require(call(gate, PSTRING_SURFACE, &mut request) == BAD_ARGUMENT as i64,
                "malformed Pascal rejected")?;
    }
    request.text = b"\xff".as_ptr();
    request.text_length = 1;
    require(call(gate, SURFACE, &mut request) == BAD_ARGUMENT as i64, "malformed UTF8 rejected")?;
    let scalars = include_bytes!("../data/system7-macroman-scalars.bin");
    for code in 0..256usize
    {
        let scalar = u32::from_le_bytes(scalars[code * 4..code * 4 + 4].try_into().unwrap());
        let ch = char::from_u32(scalar).ok_or(Failure::Mismatch("bad mapping fixture"))?;
        let mut bytes = [0; 4];
        let unicode = ch.encode_utf8(&mut bytes);
        let expected = port.text_width(0, unicode)?;
        let pstring = [1, code as u8];
        request.operation = 4;
        request.text = pstring.as_ptr();
        request.text_length = 2;
        require(call(gate, PSTRING_SURFACE, &mut request) == 0 && request.width == expected,
                "complete Apple mapping widths")?;
    }
    let mut max = [0xdb; 256];
    max[0] = 255;
    request.text = max.as_ptr();
    request.text_length = max.len();
    request.operation = 4;
    require(call(gate, PSTRING_SURFACE, &mut request) == 0,
            "255 MacRoman bytes accepted after expansion")?;
    let state = mac_toolbox::controls::StandardState::new(Rect::new(1, 1, 120, 15), native);
    system.controls().draw_standard_pixels(&mut native_pixels,
                                            128,
                                            16,
                                            mac_toolbox::controls::StandardKind::Button,
                                            state)?;
    let mut legacy = state;
    legacy.title.fill(0);
    legacy.title[..pascal.len()].copy_from_slice(pascal);
    legacy.title_length = pascal.len() as u32;
    let mut control = ControlCall { operation: DRAW_STANDARD,
                                    standard: legacy,
                                    surface: &mut surface,
                                    ..Default::default() };
    require(call(gate, PSTRING_CONTROL, &mut control) == 0, "Pascal CDEF title status")?;
    require(native_pixels == legacy_pixels, "Pascal CDEF title decoded pixels")?;
    let fonts = system.quickdraw().fonts();
    let mut font = FontCall { operation: FONT_ID,
                              text: b"\x07Chicago".as_ptr(),
                              text_length: 8,
                              ..Default::default() };
    require(call(gate, PSTRING_FONT, &mut font) == 0
            && font.selection.family == fonts.id("Chicago")?,
            "Pascal GetFNum")?;
    font.operation = FONT_NAME;
    require(call(gate, PSTRING_FONT, &mut font) == 0, "Pascal font name status")?;
    require(&font.name[..font.name_length as usize] == b"\x07Chicago", "Pascal font name output")?;
    let mut resource = ResourceCall { operation: NAMED,
                                      scope: SYSTEM,
                                      key: ResourceKey { kind: *b"FOND",
                                                         id:   0 },
                                      name: b"\x07Chicago".as_ptr(),
                                      name_length: 8,
                                      ..Default::default() };
    require(call(gate, PSTRING_RESOURCE, &mut resource) == 0, "Pascal named resource status")?;
    require(&resource.info.name[..resource.info.name_length as usize] == b"\x07Chicago",
            "Pascal resource name output")?;
    require(resource.name == b"\x07Chicago".as_ptr(), "Pascal resource input restored")?;
    let mut output = [0; 3];
    let mut expected = [0; 3];
    system.resources()
          .system()
          .get(ResourceKey { kind: *b"FOND",
                             id:   0 })?
          .read(0, &mut expected)?;
    for operation in
        [mac_abi::resources::LOAD, mac_abi::resources::RELEASE, mac_abi::resources::READ]
    {
        resource.operation = operation;
        resource.scope = SYSTEM;
        resource.output = output.as_mut_ptr();
        resource.capacity = output.len();
        require(call(gate, PSTRING_RESOURCE, &mut resource) == 0,
                "Pascal resource lifecycle status")?;
        require(&resource.info.name[..resource.info.name_length as usize] == b"\x07Chicago",
                "Pascal lifecycle metadata name")?;
        if operation == mac_abi::resources::READ
        {
            require(output == expected, "resource binary payload not transcoded")?;
        }
    }
    Ok(())
}
/// Window/log/drawing need the graphical guest. The host has no synthetic Window
/// Manager adapter; this runs on the actual resident System in the ECALL runner.
pub fn graphical(system: &System<'_>, gate: TrapGate) -> Result<(), Failure>
{
    let mut log = LogCall { bytes:  b"\x07LEGACY\n".as_ptr(),
                            length: 8 };
    require(call(gate, PSTRING_LOG, &mut log) == 0, "Pascal log")?;
    let mut request =
        WindowCall { operation: NEW_WINDOW,
                     request:   WindowRequest { id:             0,
                                                bounds:         Rect::new(100, 100, 300, 200),
                                                title:          [0; WINDOW_TITLE_BYTES],
                                                title_length:   256,
                                                minimum_width:  1,
                                                minimum_height: 1,
                                                procedure:      8,
                                                reserved:       0 } };
    request.request.title[0] = 255;
    request.request.title[1..256].fill(0xdb);
    require(call(gate, PSTRING_WINDOW, &mut request) == 0, "255-byte Pascal window creation")?;
    let id = request.request.id;
    let native = system.window(id).map_err(Failure::Service)?;
    require(native.title_length == 510, "Pascal window title expanded without truncation")?;
    require(core::str::from_utf8(&native.title[..510]).is_ok_and(|s| {
                                                          s.chars().count() == 255
                                                          && s.chars().all(|c| c == '¤')
                                                      }),
            "expanded window title Unicode")?;
    request.operation = GET_WINDOW;
    request.request.title.fill(0);
    request.request.title_length = 0;
    require(call(gate, PSTRING_WINDOW, &mut request) == 0
            && request.request.title_length == 256
            && request.request.title[..256].iter()
                                           .enumerate()
                                           .all(|(i, b)| *b == if i == 0 { 255 } else { 0xdb }),
            "Pascal window title output roundtrip")?;
    let mut draw = DrawCall { window:  id,
                              command: DrawCommand { kind:        DRAW_TEXT,
                                                     bounds:      Rect::new(2, 12, 2, 12),
                                                     color:       0,
                                                     text_length: 5,
                                                     text:        [0; INLINE_STRING_BYTES] } };
    draw.command.text[..5].copy_from_slice(b"\x04Caf\x8e");
    require(call(gate, PSTRING_DRAW, &mut draw) == 0, "Pascal retained draw")?;
    draw.command.text_length = 1;
    draw.command.text[0] = 0xff;
    require(call(gate, DRAW, &mut draw) == BAD_ARGUMENT as i64,
            "native retained draw rejects invalid UTF8")?;
    system.close_window(id).map_err(Failure::Service)?;
    let id = system.new_window(Rect::new(100, 100, 300, 200), "🦀", (1, 1), 8)
                   .map_err(Failure::Service)?;
    request.request.id = id;
    request.operation = GET_WINDOW;
    require(call(gate, PSTRING_WINDOW, &mut request) == BAD_ARGUMENT as i64,
            "unrepresentable Pascal output rejected")?;
    system.close_window(id).map_err(Failure::Service)?;
    Ok(())
}
