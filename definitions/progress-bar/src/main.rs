//! Independent progress-bar geometry and bounded change damage.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Error, Message, Part, Progress, ProgressStyle, Rect, Reply, Result, Services,
                     SurfacePort};
// Provenance: original Finder7.6.1 CODE174:0x36e..0x736, reached from
// the fvew5250 class table and its explicit item8803 branch. See the source
// hashes and colour/pattern observations in provenance/finder-copy-window.json.

fn interior(state: &Progress) -> Result<Rect>
{
    let r = state.bounds;
    let p = state.progress;
    if i32::from(r.right) - i32::from(r.left) < 3
       || i32::from(r.bottom) - i32::from(r.top) < 3
       || p.completed > p.total
       || p.in_progress > p.total - p.completed
       || (!p.determinate && (p.total != 0 || p.completed != 0 || p.in_progress != 0))
    {
        return Err(Error::BadArgument);
    }
    Ok(Rect::new(r.left + 1, r.top + 1, r.right - 1, r.bottom - 1))
}
fn edge(state: &Progress) -> Result<i16>
{
    let r = interior(state)?;
    let width = i32::from(r.right) - i32::from(r.left);
    let p = state.progress;
    Ok((i32::from(r.left)
        + if !p.determinate
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
        }) as i16)
}
fn change(before: &Progress, after: &Progress, damage: &mut [Rect; 2]) -> Result<()>
{
    let r = interior(after)?;
    let old = edge(before)?;
    let new = edge(after)?;
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
    let inside = interior(state)?;
    let settled = edge(state)?;
    let mono = state.style == ProgressStyle::Monochrome;
    port.frame_rect(state.bounds, theme.control_outline)?;
    port.paint_rect(Rect::new(inside.left, inside.top, settled, inside.bottom),
                    if mono { theme.control_outline } else { theme.progress_fill })?;
    port.paint_rect(Rect::new(settled, inside.top, inside.right, inside.bottom),
                    if mono { theme.content } else { theme.progress_track })?;
    Ok(())
}
fn handle<'a>(message: Message<'a, Progress>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port } => draw(&mut port, &state, services.theme()?)?,
        Message::ChangeBounds { before,
                                after,
                                damage } => change(&before, &after, damage)?,
        Message::Validate(state)
        | Message::CalculateRegions(state)
        | Message::Initialize(state)
        | Message::Dispose(state) =>
        {
            interior(&state)?;
        }
        Message::HitTest { state,
                           .. } =>
        {
            interior(&state)?;
            return Ok(Reply::Hit(Part::None));
        }
        Message::Track { .. } => return Err(Error::NotSupported)
    }
    Ok(Reply::Done)
}
mac_definition::define_progress_bar!(progress_bar_definition, handle);
