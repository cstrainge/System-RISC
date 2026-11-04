//! Finite real SDK workload for caller-owned pixel surfaces. This executes as
//! an ordinary no_std APPL, with either stock or packaged production CDEFs.
use alloc::vec::Vec;

use mac_toolbox::{Error, Point, Rect, System, TypeCode,
                  controls::{ScrollBoxSpec, ScrollbarState, StandardKind, StandardState}};

const WIDTH: usize = 160;
const HEIGHT: usize = 128;

fn seed(index: usize) -> u32
{
    0x550000 | ((index as u32).wrapping_mul(73) & 0xffff)
}
fn reset(pixels: &mut [u32])
{
    for (index, pixel) in pixels.iter_mut().enumerate()
    {
        *pixel = seed(index);
    }
}
fn inspect(pixels: &[u32], bounds: Rect) -> Result<u64, Error>
{
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut changed = false;
    for (index, pixel) in pixels.iter().enumerate()
    {
        hash = (hash ^ u64::from(*pixel)).wrapping_mul(0x100_0000_01b3);
        if *pixel != seed(index)
        {
            if !bounds.contains(Point { h: (index % WIDTH) as i16,
                                        v: (index / WIDTH) as i16 })
            {
                return Err(Error::BadAbi);
            }
            changed = true;
        }
    }
    if !changed
    {
        return Err(Error::BadAbi);
    }
    Ok(hash)
}
fn refused<T>(result: Result<T, Error>) -> Result<(), Error>
{
    match result
    {
        Err(Error::BadArgument) => Ok(()),
        _ => Err(Error::BadAbi)
    }
}
pub(super) fn run(system: &System<'_>, user: bool) -> Result<(), Error>
{
    let process = system.processes().current()?;
    let image = process.image.ok_or(Error::BadAbi)?;
    for id in [0, 1]
    {
        let result = system.resources()
                           .image(image)
                           .get(mac_toolbox::resources::ResourceKey { kind: TypeCode::new(*b"CDEF"),
                                                                      id });
        match (user, result)
        {
            (true, Ok(resource)) if resource.size() != 0 => (),
            (false, Err(Error::NotFound)) => (),
            _ => return Err(Error::BadAbi)
        }
    }
    let controls = system.controls();
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(WIDTH * HEIGHT).map_err(|_| Error::NoMemory)?;
    pixels.resize(WIDTH * HEIGHT, 0);
    let bounds = Rect::new(12, 12, 148, 40);
    let mut hashes = [0; 5];
    for (index, kind) in
        [StandardKind::Button, StandardKind::Checkbox, StandardKind::Radio].into_iter().enumerate()
    {
        reset(&mut pixels);
        let mut state = StandardState::new(bounds, "Native pixels");
        state.value = i16::from(kind != StandardKind::Button);
        controls.draw_standard_pixels(&mut pixels, WIDTH, HEIGHT, kind, state)?;
        hashes[index] = inspect(&pixels, bounds)?;
    }
    let before = pixels.clone();
    let mut invalid = StandardState::new(bounds, "Invalid flags");
    invalid.flags = 2;
    refused(controls.draw_standard_pixels(&mut pixels,
                                          WIDTH,
                                          HEIGHT,
                                          StandardKind::Button,
                                          invalid))?;
    if pixels != before
    {
        return Err(Error::BadAbi);
    }
    // Same valid service after refusal, with a fresh complete input surface.
    reset(&mut pixels);
    controls.draw_standard_pixels(&mut pixels,
                                  WIDTH,
                                  HEIGHT,
                                  StandardKind::Button,
                                  StandardState::new(bounds, "Native pixels"))?;
    if inspect(&pixels, bounds)? != hashes[0]
    {
        return Err(Error::BadAbi);
    }
    reset(&mut pixels);
    let bar = Rect::new(138, 8, 154, 104);
    controls.draw_scrollbar_pixels(&mut pixels,
                                   WIDTH,
                                   HEIGHT,
                                   ScrollbarState { bounds: bar,
                                                    value: 15,
                                                    maximum: 60,
                                                    active: 1,
                                                    ..Default::default() })?;
    hashes[3] = inspect(&pixels, bar)?;

    // Zero is the SDK's real offscreen SBOX constructor, with no WIND object.
    let spec = ScrollBoxSpec { viewport: Rect::new(8, 8, 136, 104),
                               scrollbar: bar,
                               count: 12,
                               ..Default::default() };
    let view = controls.create_scroll_box(mac_toolbox::windows::WindowId(0), spec)?;
    let initial = view.control().info()?;
    for row in initial.missing
    {
        view.control().set_row(row, "Native row", row == 2)?;
    }
    reset(&mut pixels);
    view.control().draw_pixels(&mut pixels, WIDTH, HEIGHT)?;
    inspect(&pixels, Rect::new(8, 8, 154, 104))?;
    let next = view.control().scroll_to(3)?;
    for row in next.missing
    {
        view.control().set_row(row, "Native row", row == 2)?;
    }
    let before_view = view.control().info()?;
    let mut invalid = spec;
    invalid.row_height = 0;
    refused(view.control().configure(invalid))?;
    let after_view = view.control().info()?;
    if before_view.spec != after_view.spec || before_view.top != after_view.top
    {
        return Err(Error::BadAbi);
    }
    reset(&mut pixels);
    view.control().draw_pixels(&mut pixels, WIDTH, HEIGHT)?;
    hashes[4] = inspect(&pixels, Rect::new(8, 8, 154, 104))?;
    let stale = view.id();
    drop(view);
    match controls.scroll_box(stale).info()
    {
        Err(Error::NotFound) => (),
        _ => return Err(Error::BadAbi)
    }
    if user
    {
        super::owned_definition(system, 2)?;
    }
    // Deferred summaries: no host acknowledgement or serial output between
    // operations. Hash comparison is finite output evidence, not a timing or
    // general pixel-perfect-compatibility claim.
    for (label, hash) in ["OFFSCREEN_BUTTON", "OFFSCREEN_CHECKBOX", "OFFSCREEN_RADIO",
                          "OFFSCREEN_SCROLLBAR", "OFFSCREEN_SCROLLBOX"].into_iter()
                                                                       .zip(hashes)
    {
        super::isolation::log_value(system, process.id, label, hash)?;
    }
    system.log("IMAGE_PROBE_OFFSCREEN_FINISHED refusals=preserved stale=refused\n")
}
