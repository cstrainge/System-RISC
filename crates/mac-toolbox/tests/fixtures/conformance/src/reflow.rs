//! Leaf reflow scenarios executed only by the real disk-loaded native fixture.
use mac_toolbox::{controls::{self, Reflow, StandardKind, StandardState},
                  *};

use super::conformance::Failure;
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn size(w: i16, h: i16) -> controls::Size
{
    controls::Size::try_from(abi::Dimensions { width:  w,
                                               height: h }).unwrap()
}
fn rect(r: Rect) -> controls::LayoutRect
{
    r.try_into().unwrap()
}
pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let mut window =
        system.windows().create_owned(windows::WindowSpec::new(Rect::new(60, 60, 380, 280),
                                                                "Reflow leaves"))?;
    window.select()?;
    let original = Rect::new(8, 8, 128, 32);
    let classic = system.controls().create_standard(window.id(),
                                                     StandardKind::WindowFontButton,
                                                     StandardState::new(original,
                                                                        "Original font bit"),
                                                     Reflow::No)?;
    require(classic.control().reflow()? == Reflow::No, "classic control unexpectedly opted in")?;
    require(matches!(classic.control().ask_size(Default::default()), Err(Error::NotSupported)),
            "unopted control accepted reflow")?;
    classic.control().draw()?;
    drop(classic);
    system.log("REFLOW_NATIVE legacy-options=no variation=window-font refusal=NotSupported\n")?;
    for kind in [StandardKind::Button,
                 StandardKind::Checkbox,
                 StandardKind::Radio,
                 StandardKind::WindowFontButton,
                 StandardKind::WindowFontCheckbox,
                 StandardKind::WindowFontRadio,
                 StandardKind::ProgressBar,
                 StandardKind::MonochromeProgressBar,
                 StandardKind::StaticText,
                 StandardKind::Table]
    {
        let passive_progress =
            matches!(kind, StandardKind::ProgressBar | StandardKind::MonochromeProgressBar);
        let mut state = StandardState::new(original,
                                           if passive_progress || kind == StandardKind::Table
                                           {
                                               ""
                                           }
                                           else
                                           {
                                               "Leaf"
                                           });
        if kind == StandardKind::StaticText
        {
            state.maximum = 0;
        }
        let owned = system.controls().create_standard(window.id(), kind, state, Reflow::Yes)?;
        exercise(system, owned.control(), original, kind == StandardKind::StaticText)?;
        require(owned.control().configure(kind, state)?.reflow == Reflow::Yes,
                "configure changed opt-in")?;
        drop(owned);
    }
    let scrollbar_bounds = Rect::new(8, 8, 24, 168);
    let owned = system.controls()
                      .create_scrollbar(window.id(),
                                        controls::ScrollbarState { bounds: scrollbar_bounds,
                                                                   maximum: 100,
                                                                   active: 1,
                                                                   ..Default::default() },
                                        1,
                                        10,
                                        Reflow::Yes)?;
    exercise(system, owned.control(), scrollbar_bounds, false)?;
    drop(owned);
    system.log("REFLOW_NATIVE leaves=buttons,window-font,progress,static,table,scrollbar \
                assignment=normal,tiny,empty cache=invalidate settings=set,clear,pair\n")?;
    let owned = system.controls().create_standard(window.id(),
                                                   StandardKind::Button,
                                                   StandardState::new(original, "Rollback"),
                                                   Reflow::Yes)?;
    let control = owned.control();
    system.windows().begin_update(window.id())?;
    let port = system.quickdraw().port(window.id());
    let mut exhausted = false;
    for _ in 0..513
    {
        match port.paint_rect(Rect::new(0, 0, 1, 1), 0xffffff)
        {
            Ok(()) => (),
            Err(Error::NoMemory) =>
            {
                exhausted = true;
                break;
            }
            Err(e) => return Err(e.into())
        }
    }
    require(exhausted, "retained limit not reached")?;
    let moved = Rect::new(16, 16, 136, 40);
    require(matches!(control.assign_rect(rect(moved)), Err(Error::NoMemory)),
            "assignment did not report native admission refusal")?;
    require(control.info()?.standard.bounds == original && control.reflow()? == Reflow::Yes,
            "failed assignment changed live state")?;
    system.windows().end_update(window.id())?;
    system.windows().begin_update(window.id())?;
    control.assign_rect(rect(moved))?;
    system.windows().end_update(window.id())?;
    let stale = control.id();
    drop(owned);
    require(matches!(system.controls().control(stale).reflow(), Err(Error::NotFound)),
            "stale handle retained layout access")?;
    system.log("REFLOW_NATIVE rollback=NoMemory geometry=unchanged retry=committed \
                stale=NotFound\n")?;
    window.close()?;
    system.log("REFLOW_NATIVE_DONE\n")?;
    Ok(())
}
fn exercise(system: &System<'_>, control: controls::ControlRef<'_>, original: Rect,
            natural_text: bool)
            -> Result<(), Failure>
{
    use abi::controls::{ControlCall, REFLOW_OBJECT, reflow as wire};
    require(control.reflow()? == Reflow::Yes, "owner query missed retained choice")?;
    let initial = control.ask_size(Default::default())?;
    let creation = if natural_text
    {
        let font = control.info()?.standard.font;
        let metrics = system.client().font(abi::quickdraw::FONT_METRICS, font, "", 0)?.metrics;
        let text = system.client().font(abi::quickdraw::FONT_WIDTH, font, "Leaf", 0)?.width;
        let dots = system.client().font(abi::quickdraw::FONT_WIDTH, font, "…", 0)?.width;
        size(text.max(dots), (metrics.ascent + metrics.descent + metrics.leading).max(1))
    }
    else
    {
        size(original.right - original.left, original.bottom - original.top)
    };
    require(initial.intrinsic.preferred() == creation,
            "CDEF missed natural text or creation size")?;
    for bounds in
        [Rect::new(12, 12, 172, 52), Rect::new(12, 12, 13, 13), Rect::new(12, 12, 12, 12), original]
    {
        control.assign_rect(rect(bounds))?;
        let info = control.info()?;
        require(info.standard.bounds == bounds || info.scrollbar.bounds == bounds,
                "assignment did not publish")?;
        require(!info.arrangement_pending && info.reflow == Reflow::Yes,
                "assignment lost metadata")?;
        control.draw()?;
        require(control.ask_size(Default::default())?.intrinsic.preferred() == creation,
                "assignment overwrote natural requirements")?;
    }
    control.set_preferred(size(40, 40))?;
    control.set_minimum(size(60, 60))?;
    require(control.ask_size(Default::default())?.intrinsic.preferred() == size(60, 60),
            "preferred was not normalized")?;
    control.clear_minimum()?;
    require(control.ask_size(Default::default())?.intrinsic.preferred() == size(40, 40),
            "clear minimum lost requested preferred")?;
    control.set_size_preferences(controls::SizePreferences { minimum:   Some(size(20, 20)),
                                                             preferred: Some(size(80, 80)) })?;
    let mut malformed = ControlCall { id: control.id().0,
                                      operation: REFLOW_OBJECT,
                                      layout: wire::Payload { message: wire::SET_MINIMUM,
                                                              size: abi::Dimensions { width:  -1,
                                                                                      height: 0 },
                                                              ..Default::default() },
                                      ..Default::default() };
    require(matches!(system.client().control(&mut malformed), Err(Error::BadArgument)),
            "negative dimensions accepted")?;
    require(control.ask_size(Default::default())?.intrinsic.preferred() == size(80, 80),
            "malformed setter mutated state")?;
    malformed.layout.message = wire::ASSIGN_RECT;
    malformed.layout.rect = Rect::new(50, 0, 10, 10);
    require(matches!(system.client().control(&mut malformed), Err(Error::BadArgument)),
            "inverted rectangle accepted")?;
    malformed.layout.message = wire::SET_CHILD;
    require(matches!(system.client().control(&mut malformed), Err(Error::NotSupported)),
            "unimplemented adoption accepted")?;
    control.clear_preferred()?;
    control.clear_minimum()?;
    control.set_visible_rect(rect(Rect::ZERO))?;
    require(!control.begin_tracking(Point { h: original.left + 2,
                                            v: original.top + 2 },
                                    0)?
                    .handled,
            "hidden control captured input")?;
    control.set_visible_rect(rect(original))?;
    control.invalidate_layout(controls::Invalidation::MeasurementAndArrangement)?;
    require(control.info()?.measurement_pending, "measurement invalidation lost")?;
    control.ask_size(Default::default())?;
    require(!control.info()?.measurement_pending, "measurement did not refresh")?;
    control.assign_rect(rect(original))?;
    require(!control.info()?.arrangement_pending, "no-change assignment stayed dirty")?;
    let mut forged = ControlCall { id: control.id().0,
                                   operation: abi::controls::CONFIGURE_OBJECT,
                                   reflow: 1,
                                   ..Default::default() };
    require(matches!(system.client().control(&mut forged), Err(Error::BadArgument)),
            "configure accepted creation option")?;
    Ok(())
}
