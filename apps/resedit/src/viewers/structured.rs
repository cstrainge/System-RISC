use super::*;
fn data(ctx: &Context<'_, '_>, out: &mut [u8]) -> ToolboxResult<usize>
{
    ctx.file.read(ctx.item.info.key, 0, out)
}
fn fill(ctx: &Context<'_, '_>, rows: core::ops::Range<u32>) -> ToolboxResult<()>
{
    let mut bytes = [0; 32768];
    let size = data(ctx, &mut bytes)?;
    let mut row = 0;
    let mut error = None;
    let mut emit = |index, line: &str| {
        if rows.contains(&index)
           && let Err(e) = ctx.row(index, line)
        {
            error = Some(e);
        }
    };
    if super::super::preview::structured(ctx.item.info.key.kind.bytes(), &bytes[..size], |line| {
           emit(row, line);
           row += 1;
       }).is_err()
    {
        emit(row, "Malformed resource; raw bytes export intact.");
    }
    if let Some(e) = error
    {
        return Err(e);
    }
    Ok(())
}
fn draw(ctx: &Context<'_, '_>) -> ToolboxResult<View>
{
    ctx.header(true)?;
    let mut bytes = [0; 32768];
    let size = data(ctx, &mut bytes)?;
    let mut count = 0;
    if super::super::preview::structured(ctx.item.info.key.kind.bytes(), &bytes[..size], |_| {
           count += 1
       }).is_err()
    {
        count += 1;
    }
    let view = ctx.prepare(count, FONT)?;
    fill(ctx, view.missing.clone())?;
    ctx.control.draw()?;
    Ok(view)
}
macro_rules! viewer {
    ($name:ident) => {
        pub struct $name;
        impl Viewer for $name
        {
            fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>
            {
                draw(ctx)
            }

            fn fill(&mut self, ctx: &Context<'_, '_>, rows: core::ops::Range<u32>)
                    -> ToolboxResult<()>
            {
                fill(ctx, rows)
            }
        }
    };
}
viewer!(StringsViewer);
viewer!(MenuViewer);
viewer!(VersionViewer);
viewer!(NativeViewer);
