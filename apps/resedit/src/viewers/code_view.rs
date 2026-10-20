use mac_toolbox::{controls::{ControlId, StandardKind, StandardState},
                  quickdraw::cursors::StandardCursor};

use super::*;
#[derive(Default)]
pub struct CodeViewer
{
    key:    Option<ResourceKey>,
    radios: Option<[ControlId; 2]>,
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

    fn radios(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<()>
    {
        if self.radios.is_some_and(|ids| {
                          ctx.system
                             .controls()
                             .control(ids[0])
                             .info()
                             .map_or(true, |info| info.window != ctx.window)
                      })
        {
            self.radios = None;
        }
        if self.radios.is_none()
        {
            let create = |hex| {
                ctx.system
                   .controls()
                   .create_standard(ctx.window, StandardKind::Radio, self.state(ctx, hex)?)
            };
            let hex = create(true)?;
            let disassembly = create(false)?;
            self.radios = Some([hex.into_id(), disassembly.into_id()]);
        }
        for (id, hex) in self.radios.unwrap().into_iter().zip([true, false])
        {
            let control = ctx.system.controls().control(id);
            control.update(StandardKind::Radio, self.state(ctx, hex)?)?;
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
        let index = self.index.as_ref().ok_or(Error::BadArgument)?;
        let source = self.source.as_ref().ok_or(Error::BadArgument)?;
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
        for (id, hex) in self.radios.unwrap_or([ControlId(0); 2]).into_iter().zip([true, false])
        {
            if id.0 == 0
            {
                continue;
            }
            let mut accepted = false;
            if ctx.system.controls().control(id).track(point, |_| {
                                                     accepted = true;
                                                     Ok(())
                                                 })?
            {
                if accepted && self.hex != hex
                {
                    self.hex = hex;
                    ctx.control.scroll_to(0)?;
                    return Ok(Interaction::Redraw);
                }
                return Ok(Interaction::None);
            }
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
