use super::*;
pub struct HexViewer;
impl Viewer for HexViewer
{
    fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>
    {
        ctx.header(true)?;
        let view =
            ctx.prepare(ctx.item.info.length.div_ceil(8).min(u32::MAX as usize) as u32, HEX_FONT)?;
        self.fill(ctx, view.missing.clone())?;
        ctx.control.draw()?;
        Ok(view)
    }

    fn fill(&mut self, ctx: &Context<'_, '_>, rows: core::ops::Range<u32>) -> ToolboxResult<()>
    {
        let mut data = [0; 4096];
        let mut first = rows.start;
        while first < rows.end
        {
            let count = (rows.end - first).min((data.len() / 8) as u32);
            let size =
                ctx.file
                   .read(ctx.item.info.key, first as usize * 8, &mut data[..count as usize * 8])?;
            let mut row = first;
            let mut error = None;
            super::super::preview::hex(&data[..size], first as usize * 8, |line| {
                if let Err(e) = ctx.row(row, line)
                {
                    error = Some(e);
                }
                row += 1;
            });
            if let Some(e) = error
            {
                return Err(e);
            }
            first += count;
        }
        Ok(())
    }
}
