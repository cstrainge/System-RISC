//! Independent progress-bar geometry and bounded change damage.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Error, Message, Part, Progress, ProgressStyle, Rect, Reply, Result, Services,
                     SurfacePort};
// Provenance: original Finder7.6.1 CODE174:0x36e..0x736, reached from
// the fvew5250 class table and its explicit item8803 branch. See the source
// hashes and colour/pattern observations in provenance/finder-copy-window.json.

struct Geometry
{
    interior: Rect,
    edge:     i16
}
fn geometry(state: &Progress) -> Result<Geometry>
{
    let r = state.bounds;
    validate_progress(state)?;
    let p = state.progress;
    if small(r)
    {
        return Err(Error::BadArgument);
    }
    let interior = Rect::new(r.left + 1, r.top + 1, r.right - 1, r.bottom - 1);
    let width = i32::from(interior.right) - i32::from(interior.left);
    let filled = if !p.determinate
    {
        0
    }
    else if p.total == 0
    {
        width
    }
    else
    {
        (width as u128 * u128::from(p.completed) / u128::from(p.total)) as i32
    };
    Ok(Geometry { interior,
                  edge: (i32::from(interior.left) + filled) as i16 })
}
fn change(before: &Progress, after: &Progress, damage: &mut [Rect; 2]) -> Result<()>
{
    let old = geometry(before)?.edge;
    let next = geometry(after)?;
    let r = next.interior;
    let new = next.edge;
    *damage = [Rect::ZERO; 2];
    if before.bounds != after.bounds
    {
        *damage = [after.bounds, Rect::ZERO];
    }
    else if old != new
    {
        damage[0] = Rect::new(old.min(new), r.top, old.max(new), r.bottom);
    }
    Ok(())
}
fn draw(port: &mut SurfacePort<'_>, state: &Progress, theme: mac_definition::Theme) -> Result<()>
{
    let Geometry { interior: inside,
                   edge: settled } = geometry(state)?;
    let mono = state.style == ProgressStyle::Monochrome;
    port.frame_rect(state.bounds, theme.control_outline)?;
    port.paint_rect(Rect::new(inside.left, inside.top, settled, inside.bottom),
                    if mono { theme.control_outline } else { theme.progress_fill })?;
    port.paint_rect(Rect::new(settled, inside.top, inside.right, inside.bottom),
                    if mono { theme.content } else { theme.progress_track })?;
    Ok(())
}
#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, Progress>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Reflow { request,
                          creation,
                          .. } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            return mac_definition::leaf_reflow(request, creation, mac_definition::reflow::Size::try_from(mac_abi::Dimensions { width: 3, height: 3 }).map_err(|_| Error::BadAbi)?);
        }
        Message::Draw { state,
                        mut port } =>
        {
            if services.reflow() == mac_definition::Reflow::Yes && small(state.bounds)
            {
                port.frame_rect(state.bounds, services.theme()?.control_outline)?;
            }
            else
            {
                draw(&mut port, &state, services.theme()?)?;
            }
        }
        Message::ChangeBounds { before,
                                after,
                                damage } =>
        {
            if services.reflow() == mac_definition::Reflow::Yes
               && (small(before.bounds) || small(after.bounds))
            {
                *damage = [before.bounds, after.bounds];
            }
            else
            {
                change(&before, &after, damage)?;
            }
        }
        Message::Validate(state)
        | Message::CalculateRegions(state)
        | Message::Initialize(state)
        | Message::Dispose(state) =>
        {
            if services.reflow() == mac_definition::Reflow::Yes
            {
                validate_progress(&state)?;
            }
            else
            {
                geometry(&state)?;
            }
        }
        Message::HitTest { state,
                           .. } =>
        {
            if services.reflow() == mac_definition::Reflow::Yes
            {
                validate_progress(&state)?;
            }
            else
            {
                geometry(&state)?;
            }
            return Ok(Reply::Hit(Part::None));
        }
        Message::Track { .. } => return Err(Error::NotSupported)
    }
    Ok(Reply::Done)
}

fn small(bounds: Rect) -> bool
{
    i32::from(bounds.right) - i32::from(bounds.left) < 3
    || i32::from(bounds.bottom) - i32::from(bounds.top) < 3
}

fn validate_progress(state: &Progress) -> Result<()>
{
    let p = state.progress;
    if p.completed > p.total
       || p.in_progress > p.total - p.completed
       || (!p.determinate && (p.total != 0 || p.completed != 0 || p.in_progress != 0))
    {
        return Err(Error::BadArgument);
    }
    Ok(())
}
