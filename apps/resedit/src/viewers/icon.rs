use super::*;
pub struct IconViewer;
impl Viewer for IconViewer
{
    fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>
    {
        ctx.header(true)?;
        let view = ctx.prepare(0, FONT)?;
        ctx.control.draw()?;
        let mut data = [0; 32768];
        let size = ctx.file.read(ctx.item.info.key, 0, &mut data)?;
        let port = ctx.system.quickdraw().graphics(ctx.window);
        port.clip_rect(ctx.viewport())?;
        if port.draw_icon(ctx.item.info.key.kind.bytes(),
                          &data[..size],
                          Rect::new(ctx.area.left + 19,
                                    ctx.area.top + 91,
                                    ctx.area.left + 147,
                                    ctx.area.top + 219))
               .is_err()
        {
            ctx.text(4, 93, "Icon preview unavailable")?;
        }
        port.reset()?;
        Ok(view)
    }

    fn fill(&mut self, _: &Context<'_, '_>, _: core::ops::Range<u32>) -> ToolboxResult<()>
    {
        Ok(())
    }

    fn mouse(&mut self, _: &Context<'_, '_>, _: Point) -> ToolboxResult<Interaction>
    {
        Ok(Interaction::None)
    }
}
