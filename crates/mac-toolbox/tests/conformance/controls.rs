//! Original CDEF pixels and TestControl records exercised through aggregate SDK.
use mac_toolbox::{System,
                  controls::{ScrollbarState, StandardKind, StandardState},
                  quickdraw::{Point, Rect}};

use super::{Failure, require};
fn u16_at(data: &[u8], at: usize) -> Result<u16, Failure>
{
    let bytes = data.get(at..at + 2).ok_or(Failure::Mismatch("truncated control fixture"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32, Failure>
{
    Ok((u16_at(data, at)? as u32) << 16 | u16_at(data, at + 2)? as u32)
}
fn kind(code: u16) -> Result<StandardKind, Failure>
{
    match code
    {
        0 => Ok(StandardKind::Button),
        1 => Ok(StandardKind::Checkbox),
        2 => Ok(StandardKind::Radio),
        8 => Ok(StandardKind::WindowFontButton),
        9 => Ok(StandardKind::WindowFontCheckbox),
        10 => Ok(StandardKind::WindowFontRadio),
        _ => Err(Failure::Mismatch("invalid original control procedure"))
    }
}
/// Compare all54 whole control rectangles (155,520pixels), with no decoration masks.
pub fn original_control_pixels(system: &System<'_>) -> Result<usize, Failure>
{
    let data = include_bytes!("../data/original-control-pixels.bin");
    require(&data[..4] == b"CDF0", "original control fixture magic")?;
    let count = u16_at(data, 4)? as usize;
    let mut at = 6;
    let mut pixels = [0x00ff00; 160 * 24];
    for case in 0..count
    {
        let procedure = u16_at(data, at)?;
        let width = u16_at(data, at + 2)? as usize;
        let height = u16_at(data, at + 4)? as usize;
        let mut state =
            StandardState::new(Rect::new(0, 0, width as i16, height as i16), "RISCy System");
        state.value = u16_at(data, at + 6)? as i16;
        state.highlighted = u16_at(data, at + 8)? as u32;
        state.font.family = u16_at(data, at + 10)? as i16;
        state.font.size = u16_at(data, at + 12)? as i16;
        let palette_count = u16_at(data, at + 14)? as usize;
        let encoded_length = u32_at(data, at + 16)? as usize;
        at += 20;
        let palette = data.get(at..at + palette_count * 3)
                          .ok_or(Failure::Mismatch("truncated control palette"))?;
        at += palette_count * 3;
        let encoded = data.get(at..at + encoded_length)
                          .ok_or(Failure::Mismatch("truncated original control pixels"))?;
        at += encoded_length;
        let pixels = pixels.get_mut(..width * height)
                           .ok_or(Failure::Mismatch("oversized original control"))?;
        pixels.fill(0xffffff);
        system.controls().draw_standard_pixels(pixels, width, height, kind(procedure)?, state)?;
        let mut index = 0;
        for run in encoded.as_chunks::<3>().0
        {
            let length = u16::from_be_bytes([run[0], run[1]]) as usize;
            let p = run[2] as usize * 3;
            let rgb = palette.get(p..p + 3).ok_or(Failure::Mismatch("original palette index"))?;
            let expected = ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32;
            for _ in 0..length
            {
                let actual =
                    *pixels.get(index).ok_or(Failure::Mismatch("original pixel run overflow"))?;
                if actual != expected
                {
                    return Err(Failure::ControlPixel { case: case as u16,
                                                       x: (index % width) as u16,
                                                       y: (index / width) as u16,
                                                       actual,
                                                       expected });
                }
                index += 1;
            }
        }
        require(encoded.len().is_multiple_of(3) && index == pixels.len(),
                "original control run length")?;
    }
    require(at == data.len(), "original control trailing bytes")?;
    Ok(count)
}
/// Original972enabled +81disabled observations per control, including repeated
/// capture passes. Hit geometry remains in the actual CDEF resource.
pub fn original_control_hits(system: &System<'_>) -> Result<usize, Failure>
{
    let data = include_bytes!("../data/original-control-hits.bin");
    require(&data[..4] == b"CDHT", "original hit fixture magic")?;
    let count = u16_at(data, 4)? as usize;
    require(data.len() == 6 + count * 8, "original hit fixture length")?;
    for (case, row) in data[6..].as_chunks::<8>().0.iter().enumerate()
    {
        let page = row[0];
        let control = row[1];
        let x = u16_at(row, 2)? as i16;
        let y = u16_at(row, 4)? as i16;
        let expected = u16_at(row, 6)? as i16;
        let (width, height) = match control
        {
            0 | 5 => (120, 24),
            1 | 2 | 6 | 7 => (160, 18),
            3 => (16, 220),
            4 => (340, 16),
            _ => return Err(Failure::Mismatch("invalid original control index"))
        };
        let bounds = Rect::new(0, 0, width, height);
        let point = Point { h: x,
                            v: y };
        let actual = if control == 3 || control == 4
        {
            system.controls().scrollbar_hit(mac_toolbox::windows::WindowId(0),
                                             bounds,
                                             50,
                                             100,
                                             page != 6,
                                             point)? as i16
        }
        else
        {
            let mut state = StandardState::new(bounds, "RISCy System");
            state.highlighted = if page == 6 { 255 } else { 0 };
            state.font.family = 3;
            state.font.size = 9;
            system.controls().standard_hit(kind(if control < 3
                                                 {
                                                     control as u16
                                                 }
                                                 else
                                                 {
                                                     control as u16 + 3
                                                 })?,
                                            state,
                                            point)? as i16
        };
        if actual != expected
        {
            return Err(Failure::ControlHit { case: case as u16,
                                             actual,
                                             expected });
        }
    }
    Ok(count)
}
/// Native safe transport contract: invalid pixel geometry fails before drawing.
pub fn control_pixel_bounds(system: &System<'_>) -> Result<(), Failure>
{
    let mut pixels = [0x123456; 8];
    let state = StandardState::new(Rect::new(0, 0, 8, 1), "A");
    require(system.controls().draw_standard_pixels(&mut pixels,
                                                   8,
                                                   2,
                                                   StandardKind::Button,
                                                   state)
            == Err(mac_toolbox::Error::BadArgument),
            "control pixel storage bounds")?;
    require(pixels == [0x123456; 8], "failed control pixel storage untouched")?;
    let scrollbar = ScrollbarState { bounds: Rect::new(0, 0, 15, 45),
                                     value: 0,
                                     maximum: 100,
                                     active: 1,
                                     ..Default::default() };
    require(system.controls().draw_scrollbar_pixels(&mut pixels, 0, 0, scrollbar)
            == Err(mac_toolbox::Error::BadArgument),
            "empty control pixel dimensions")?;
    let mut unsupported = state;
    unsupported.font.family = 20;
    unsupported.font.size = 18;
    require(system.controls().draw_standard_pixels(&mut pixels,
                                                   8,
                                                   1,
                                                   StandardKind::WindowFontButton,
                                                   unsupported)
            == Err(mac_toolbox::Error::NotSupported),
            "unsupported CDEF font rejected before drawing")?;
    unsupported = state;
    unsupported.title[0] = 0xff;
    unsupported.title_length = 1;
    require(system.controls().draw_standard_pixels(&mut pixels,
                                                   8,
                                                   1,
                                                   StandardKind::Button,
                                                   unsupported)
            == Err(mac_toolbox::Error::BadArgument),
            "malformed control title rejected before drawing")?;
    let unsupported = ScrollbarState { bounds: Rect::new(0, 0, 14, 40),
                                       ..scrollbar };
    require(system.controls().draw_scrollbar_pixels(&mut pixels, 8, 1, unsupported)
            == Err(mac_toolbox::Error::NotSupported),
            "unsupported scrollbar geometry rejected before drawing")?;
    require(pixels == [0x123456; 8], "definition rejection preserves borrowed pixels")
}

/// All32 original scrollbar rectangles, preserving global checker-pattern phase
/// while rendering into bounded borrowed pixels. Every color/border is checked.
pub fn original_scrollbar_pixels(system: &System<'_>) -> Result<usize, Failure>
{
    let data = include_bytes!("../data/original-scrollbar-pixels.bin");
    require(&data[..4] == b"CDF1", "original scrollbar fixture magic")?;
    let count = u16_at(data, 4)? as usize;
    let mut at = 6;
    let mut storage = [0xffffff; 8192];
    for case in 0..count
    {
        let width = u16_at(data, at)? as usize;
        let height = u16_at(data, at + 2)? as usize;
        let left = u16_at(data, at + 4)? as usize;
        let top = u16_at(data, at + 6)? as usize;
        let state = ScrollbarState { bounds:      Rect::new(left as i16,
                                                            top as i16,
                                                            (left + width) as i16,
                                                            (top + height) as i16),
                                     value:       u16_at(data, at + 8)? as i16,
                                     maximum:     u16_at(data, at + 10)? as i16,
                                     highlighted: u16_at(data, at + 12)? as i16,
                                     active:      u16_at(data, at + 14)? as u32,
                                     reserved:    0 };
        let palette_count = u16_at(data, at + 16)? as usize;
        let encoded_length = u32_at(data, at + 18)? as usize;
        at += 22;
        let palette = data.get(at..at + palette_count * 3)
                          .ok_or(Failure::Mismatch("truncated scrollbar palette"))?;
        at += palette_count * 3;
        let encoded = data.get(at..at + encoded_length)
                          .ok_or(Failure::Mismatch("truncated scrollbar pixels"))?;
        at += encoded_length;
        let row_width = left + width;
        let buffer_height = top + height;
        let pixels = storage.get_mut(..row_width * buffer_height)
                            .ok_or(Failure::Mismatch("oversized scrollbar fixture"))?;
        pixels.fill(0x123456);
        system.controls().draw_scrollbar_pixels(pixels, row_width, buffer_height, state)?;
        let mut index = 0;
        for run in encoded.as_chunks::<3>().0
        {
            let length = u16::from_be_bytes([run[0], run[1]]) as usize;
            let rgb = palette.get(run[2] as usize * 3..run[2] as usize * 3 + 3)
                             .ok_or(Failure::Mismatch("scrollbar palette index"))?;
            let expected = ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32;
            for _ in 0..length
            {
                require(index < width * height, "scrollbar run overflow")?;
                let x = index % width;
                let y = index / width;
                let actual = pixels[(y + top) * row_width + x + left];
                if actual != expected
                {
                    return Err(Failure::ControlPixel { case: case as u16 + 54,
                                                       x: x as u16,
                                                       y: y as u16,
                                                       actual,
                                                       expected });
                }
                index += 1;
            }
        }
        require(encoded.len().is_multiple_of(3) && index == width * height,
                "original scrollbar run length")?;
        for y in 0..buffer_height
        {
            for x in 0..row_width
            {
                if x < left || y < top
                {
                    require(pixels[y * row_width + x] == 0x123456,
                            "control outside rectangle untouched")?;
                }
            }
        }
    }
    require(at == data.len(), "trailing scrollbar fixture data")?;
    Ok(count)
}

/// Exercise the migrated progress and static-text families through loaded CDEFs.
/// These checks formerly ran against host-linked definitions.
pub fn author_families(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<(), Failure>
{
    use mac_toolbox::{Error, abi::controls::ProgressRange, quickdraw::SurfacePort};
    let theme = mac_toolbox::quickdraw::theme::current(system.client())?;
    let bounds = Rect::new(2, 2, 24, 10);
    for (kind, fill, track) in
        [(StandardKind::ProgressBar, theme.progress_fill, theme.progress_track),
         (StandardKind::MonochromeProgressBar, theme.control_outline, theme.content)]
    {
        let mut pixels = [0xff00ff; 26 * 12];
        for (completed, in_progress, total, edge) in [(0, 10, 20, 3),
                                                      (10, 5, 20, 13),
                                                      (20, 0, 20, 23),
                                                      (0, 0, 20, 3),
                                                      (u64::MAX / 2, 0, u64::MAX, 12),
                                                      (0, 0, 0, 23)]
        {
            let state = StandardState { bounds,
                                        progress: ProgressRange { completed,
                                                                  in_progress,
                                                                  total,
                                                                  determinate: 1,
                                                                  reserved: 0 },
                                        ..Default::default() };
            system.controls().draw_standard_pixels(&mut pixels, 26, 12, kind, state)?;
            for y in 0..12
            {
                for x in 0..26
                {
                    let expected = if !(2..24).contains(&x) || !(2..10).contains(&y)
                    {
                        0xff00ff
                    }
                    else if x == 2 || x == 23 || y == 2 || y == 9
                    {
                        theme.control_outline
                    }
                    else if x < edge
                    {
                        fill
                    }
                    else
                    {
                        track
                    };
                    require(pixels[y * 26 + x] == expected,
                            "loaded progress CDEF frame/fill/clipping")?;
                }
            }
        }
        let before = pixels;
        let invalid = StandardState { bounds,
                                      progress: ProgressRange { completed: 2,
                                                                total: 1,
                                                                determinate: 1,
                                                                ..Default::default() },
                                      ..Default::default() };
        let error =
            system.controls().draw_standard_pixels(&mut pixels, 26, 12, kind, invalid).err();
        require(error == Some(Error::BadArgument), "loaded progress CDEF invalid range cause")?;
        struct Diagnostic
        {
            bytes:  [u8; 64],
            length: usize
        }
        impl core::fmt::Write for Diagnostic
        {
            fn write_str(&mut self, text: &str) -> core::fmt::Result
            {
                let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
                let target = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
                target.copy_from_slice(text.as_bytes());
                self.length = end;
                Ok(())
            }
        }
        let mut diagnostic = Diagnostic { bytes:  [0; 64],
                                          length: 0 };
        if let Some(error) = error
        {
            core::fmt::write(&mut diagnostic, format_args!("{error}"))
                .map_err(|_| Failure::Mismatch("progress diagnostic overflow"))?;
        }
        require(&diagnostic.bytes[..diagnostic.length] == b"invalid argument",
                "invalid progress diagnostic")?;
        require(pixels == before, "invalid progress changed pixels")?;
        let valid = StandardState { bounds,
                                    progress: ProgressRange { total: 1,
                                                              determinate: 1,
                                                              ..Default::default() },
                                    ..Default::default() };
        system.controls().draw_standard_pixels(&mut pixels, 26, 12, kind, valid)?;
    }
    let width = 96;
    let height = 32;
    let bounds = Rect::new(4, 4, 84, 15);
    for (text, enabled) in [("", true), ("Glyph gj", true), ("Disabled", false)]
    {
        let mut state = StandardState::new(bounds, text);
        state.maximum = 0;
        state.enabled = u32::from(enabled);
        let mut actual = [0x314159; 96 * 32];
        system.controls().draw_standard_pixels(&mut actual,
                                                width,
                                                height,
                                                StandardKind::StaticText,
                                                state)?;
        let mut expected = [0x314159; 96 * 32];
        let metrics = system.quickdraw().fonts().metrics(state.font)?;
        let mut surface = mac_abi::Surface { pixels: expected.as_mut_ptr(),
                                             width:  width as u32,
                                             height: height as u32,
                                             origin: Point::default(),
                                             clip:   bounds };
        // This is a real resident surface service, not a replacement definition entry.
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) };
        let mut port = SurfacePort::new(connection);
        port.font_text_clipped(state.font,
                               Point { h: bounds.left,
                                       v: bounds.top + metrics.ascent },
                               text,
                               bounds.right - bounds.left,
                               if enabled { theme.text } else { theme.disabled_text })?;
        require(actual == expected, "loaded static-text CDEF transparency/glyph clipping")?;
    }
    system.log("CDEF_FAMILIES_USERSPACE_PASS progress=12 static_text=3 range_refusal=passed \
                retry=passed\n")?;
    Ok(())
}
