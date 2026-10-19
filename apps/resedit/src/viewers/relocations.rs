use super::*;
pub struct RelocationsViewer;
impl Viewer for RelocationsViewer
{
    fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>
    {
        ctx.header(true)?;
        let count = super::super::code::relocation_count(ctx.item.info.length, |at, out| {
                        ctx.file.read(ctx.item.info.key, at, out)
                    }).unwrap_or(1);
        let view = ctx.prepare(count, HEX_FONT)?;
        self.fill(ctx, view.missing.clone())?;
        ctx.control.draw()?;
        Ok(view)
    }

    fn fill(&mut self, ctx: &Context<'_, '_>, rows: core::ops::Range<u32>) -> ToolboxResult<()>
    {
        let mut row = rows.start;
        let mut error = None;
        if super::super::code::relocations(ctx.item.info.length,
                                           rows.start,
                                           rows.end - rows.start,
                                           |at, out| ctx.file.read(ctx.item.info.key, at, out),
                                           |line| {
                                               if let Err(e) = ctx.row(row, line)
                                               {
                                                   error = Some(e);
                                               }
                                               row += 1;
                                           }).is_err()
        {
            for row in rows
            {
                ctx.row(row, "Malformed RLOC; raw bytes export intact.")?;
            }
        }
        if let Some(e) = error
        {
            return Err(e);
        }
        Ok(())
    }
}
