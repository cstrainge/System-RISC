use mac_toolbox::{controls::{Part, StandardKind, StandardState},
                  quickdraw::cursors::StandardCursor};

use super::*;
#[derive(Default)]
pub struct CodeViewer
{
    key:    Option<ResourceKey>,
    index:  Option<super::super::code::Index>,
    hex:    bool,
    source: Option<super::super::code::Source>
}
impl CodeViewer
{
    fn radio(ctx: &Context<'_, '_>, hex: bool) -> Rect
    {
        let left = ctx.area.left + 4 + if hex { 0 } else { 112 };
        Rect::new(left,
                  ctx.area.top + 2,
                  if hex { left + 104 } else { ctx.area.right - 2 },
                  ctx.area.top + 23)
    }

    fn state(&self, ctx: &Context<'_, '_>, hex: bool) -> ToolboxResult<StandardState>
    {
        let mut state = StandardState::new(Self::radio(ctx, hex),
                                           super::super::ui(ctx.system,
                                                            if hex { 19 } else { 20 },
                                                            &mut [0; 128])?);
        state.value = i16::from(self.hex == hex);
        state.enabled = u32::from(hex || self.index.is_some());
        Ok(state)
    }

    fn radios(&self, ctx: &Context<'_, '_>) -> ToolboxResult<()>
    {
        for hex in [true, false]
        {
            ctx.system
               .controls()
               .draw_standard_state(ctx.window, StandardKind::Radio, self.state(ctx, hex)?)?;
        }
        Ok(())
    }
}
impl Viewer for CodeViewer
{
    fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>
    {
        if self.key != Some(ctx.item.info.key)
        {
            self.key = Some(ctx.item.info.key);
            ctx.system.quickdraw().cursors().set_standard(StandardCursor::Watch)?;
            self.source = super::super::code::Source::open(ctx.file, ctx.item.info.key,
                                                           ctx.item.info.length).ok();
            self.index = self.source.as_ref().and_then(|source| {
                                                 super::super::code::Index::build(source.length,
                                                                         &source.descriptor,
                                                                         |at, out| {
                                                                             source.read(ctx.file,
                                                                                         at, out)
                                                                         }).ok()
                                             });
            self.hex = self.index.is_none();
            ctx.system.quickdraw().cursors().init()?;
        }
        ctx.header(false)?;
        self.radios(ctx)?;
        let count = if self.hex
        {
            ctx.item.info.length.div_ceil(8).min(u32::MAX as usize) as u32
        }
        else
        {
            self.index.as_ref().unwrap().rows
        };
        // The immutable loader header is outside the managed scroll viewport.
        let header_height = super::super::code::HEADER_ROWS as i16 * 14;
        let mut row = 0;
        let mut error = None;
        if let Some(source) = &self.source
        {
            super::super::code::loader(ctx.file, source, |line| {
                if let Err(e) =
                    ctx.system.quickdraw().graphics(ctx.window).text(HEX_FONT,
                                                                     Point { h: ctx.area.left + 4,
                                                                             v: ctx.area.top
                                                                                + 93
                                                                                + row * 14 },
                                                                     line,
                                                                     0)
                {
                    error = Some(e);
                }
                row += 1;
            });
        }
        else
        {
            ctx.text(4, 93, "Legacy code: hex only")?;
        }
        if let Some(e) = error
        {
            return Err(e);
        }
        let mut spec = ctx.spec(count, HEX_FONT);
        spec.viewport.top += header_height;
        spec.scrollbar.top += header_height;
        ctx.control.configure(spec)?;
        let view = ctx.control.invalidate()?;
        self.fill(ctx, view.missing.clone())?;
        ctx.control.draw()?;
        Ok(view)
    }

    fn fill(&mut self, ctx: &Context<'_, '_>, rows: core::ops::Range<u32>) -> ToolboxResult<()>
    {
        if self.hex
        {
            return hex::HexViewer.fill(ctx, rows);
        }
        let index = self.index.as_ref().ok_or(Error(BAD_ARGUMENT))?;
        let source = self.source.as_ref().ok_or(Error(BAD_ARGUMENT))?;
        let mut row = rows.start;
        let mut error = None;
        index.lines(rows.start,
                    rows.end - rows.start,
                    |at, out| source.read(ctx.file, at, out),
                    |line| {
                        if let Err(e) = ctx.row(row, line)
                        {
                            error = Some(e);
                        }
                        row += 1;
                    })?;
        if let Some(e) = error
        {
            return Err(e);
        }
        Ok(())
    }

    fn mouse(&mut self, ctx: &Context<'_, '_>, point: Point) -> ToolboxResult<Interaction>
    {
        for hex in [true, false]
        {
            let mut state = self.state(ctx, hex)?;
            if ctx.system.controls().standard_hit(StandardKind::Radio, state, point)? != Part::Check
            {
                continue;
            }
            let origin = ctx.system.windows().info(ctx.window)?.bounds;
            let mut highlight = None;
            let accepted = loop
            {
                let input = ctx.system.events().input_state()?;
                let point = Point { h: input.mouse.h - origin.left,
                                    v: input.mouse.v - origin.top };
                let inside =
                    ctx.system.controls().standard_hit(StandardKind::Radio, state, point)?
                    == Part::Check;
                let down = input.modifiers & events::BTN_STATE == 0;
                if highlight != Some(inside && down)
                {
                    state.highlighted = u32::from(inside && down);
                    ctx.system
                       .controls()
                       .draw_standard_state(ctx.window, StandardKind::Radio, state)?;
                    // Present this animation state before acknowledging it. A
                    // zero-mask poll preserves queued application events.
                    ctx.system.events().wait_event(0, 0)?;
                    let mut log = Text::<96>::new();
                    let _ = writeln!(log,
                                     "RESEDIT_RADIO_READY hex={} pressed={}",
                                     u32::from(hex),
                                     state.highlighted);
                    ctx.system.log(log.as_str()).map_err(Error)?;
                    highlight = Some(inside && down);
                }
                if !down
                {
                    break inside;
                }
                ctx.system.events().wait_event(0, 1)?;
            };
            if accepted && self.hex != hex
            {
                self.hex = hex;
                ctx.control.scroll_to(0)?;
                return Ok(Interaction::Redraw);
            }
            return Ok(Interaction::None);
        }
        if ctx.control.info()?.spec.scrollbar.contains(point)
        {
            Ok(Interaction::Scroll(point))
        }
        else
        {
            Ok(Interaction::None)
        }
    }

    fn mode(&self) -> &'static str
    {
        if self.hex { "preview" } else { "disassembly" }
    }
}
