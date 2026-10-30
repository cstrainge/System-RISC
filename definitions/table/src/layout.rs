//! Deterministic one-axis grid policy. Two such calculations resolve columns
//! before width-dependent row measurement. No service calls or allocation.
use mac_definition::reflow::{AxisConstraint, Error, Result, TrackSize, grid::AxisCalculation};
fn add(a: i64, b: i64) -> Result<i64>
{
    a.checked_add(b).ok_or(Error::ExtentOverflow)
}
fn fraction(total: i64, part: i64, whole: i64) -> Result<i64>
{
    if whole == 0
    {
        return Ok(0);
    }
    i64::try_from(i128::from(total) * i128::from(part) / i128::from(whole)).map_err(|_| {
                                                                               Error::ExtentOverflow
                                                                           })
}
fn total(c: &AxisCalculation<'_>, field: impl Fn(mac_definition::reflow::grid::Track) -> i64)
         -> Result<i64>
{
    let mut sum = 0;
    for i in 0..c.track_count()
    {
        sum = add(sum, field(c.track(i)?))?;
    }
    Ok(sum)
}
/// Single-track requirements first, then spans in stable cell order. A span's
/// deficit is split evenly, with earlier tracks receiving remainder pixels.
fn requirements(c: &mut AxisCalculation<'_>, preferred: bool) -> Result<()>
{
    for spanning in [false, true]
    {
        for i in 0..c.item_count()
        {
            let item = c.item(i)?;
            if (item.span > 1) != spanning
            {
                continue;
            }
            let mut sum = 0;
            for j in item.start..item.start + item.span
            {
                let t = c.track(j)?;
                sum = add(sum, if preferred { t.preferred } else { t.minimum })?;
            }
            let need = if preferred { item.preferred } else { item.minimum };
            let deficit = (need - sum).max(0);
            for (offset, j) in (item.start..item.start + item.span).enumerate()
            {
                let t = c.track(j)?;
                let extra = deficit / item.span as i64
                            + i64::from((offset as i64) < deficit % item.span as i64);
                let minimum = if preferred { t.minimum } else { add(t.minimum, extra)? };
                let preferred =
                    if preferred { add(t.preferred, extra)? } else { t.preferred.max(minimum) };
                c.set_track(j, minimum, preferred, 0)?;
            }
        }
    }
    Ok(())
}
/// Proportional allocation with cumulative rounding spends exactly the admitted
/// budget. It needs one pass, including shortage and uneven pixel budgets.
fn grow(c: &mut AxisCalculation<'_>, budget: i64,
        weight: impl Fn(mac_definition::reflow::grid::Track) -> i64)
        -> Result<i64>
{
    let denominator = total(c, &weight)?;
    let spend = budget.min(denominator);
    let mut prefix = 0;
    let mut previous = 0;
    for i in 0..c.track_count()
    {
        let t = c.track(i)?;
        prefix = add(prefix, weight(t))?;
        let end = fraction(spend, prefix, denominator)?;
        c.set_track(i, t.minimum, t.preferred, add(t.assigned, end - previous)?)?;
        previous = end;
    }
    Ok(budget - spend)
}
pub fn calculate(c: &mut AxisCalculation<'_>) -> Result<()>
{
    for i in 0..c.track_count()
    {
        c.set_track(i, 0, 0, 0)?;
    }
    requirements(c, false)?;
    requirements(c, true)?;
    let minimum = total(c, |t| t.minimum)?;
    let preferred = total(c, |t| t.preferred)?;
    let budget = match c.constraint()
    {
        AxisConstraint::Unbounded => preferred,
        AxisConstraint::AtMost(v) => preferred.min(i64::from(v.pixels())),
        AxisConstraint::Exact(v) => i64::from(v.pixels())
    };
    if budget < minimum
    {
        // Explicit forced-size policy: scale minima, retaining the true minimum
        // in the reply. Children safely degrade, even to zero-area rectangles.
        grow(c, budget, |t| t.minimum)?;
    }
    else
    {
        for i in 0..c.track_count()
        {
            let t = c.track(i)?;
            c.set_track(i, t.minimum, t.preferred, t.minimum)?;
        }
        let mut left = budget - minimum;
        // Percent is a target of the offered budget, never a replacement for a
        // minimum. Competing percent deficits share remaining space fairly.
        left = grow(c, left, |t| {
            match t.size
            {
                TrackSize::Percent(p) =>
                {
                    // Validated p <= 10_000 keeps the result within budget;
                    // multiply in i128 before the proven nonnegative narrowing.
                    let target = (i128::from(budget) * i128::from(p) / 10_000) as i64;
                    (target - t.assigned).max(0)
                }
                _ => 0
            }
        })?;
        left = grow(c, left, |t| {
            if t.size == TrackSize::Fit { (t.preferred - t.assigned).max(0) } else { 0 }
        })?;
        let fills = total(c, |t| i64::from(t.size == TrackSize::Fill))?;
        if fills > 0
        {
            let mut ordinal = 0;
            let mut previous = 0;
            for i in 0..c.track_count()
            {
                let t = c.track(i)?;
                if t.size == TrackSize::Fill
                {
                    ordinal += 1;
                    let end = fraction(left, ordinal, fills)?;
                    c.set_track(i, t.minimum, t.preferred, add(t.assigned, end - previous)?)?;
                    previous = end;
                }
            }
        }
    }
    let mut position = 0;
    for i in 0..c.track_count()
    {
        c.set_position(i, position)?;
        position = add(position, c.track(i)?.assigned)?;
    }
    for i in 0..c.item_count()
    {
        let item = c.item(i)?;
        let start = c.track(item.start)?.position;
        let last = c.track(item.start + item.span - 1)?;
        let space = add(last.position, last.assigned)? - start;
        let extent = if item.leading && item.trailing { space } else { item.preferred.min(space) };
        let offset = if item.leading
        {
            0
        }
        else if item.trailing
        {
            space - extent
        }
        else
        {
            (space - extent) / 2
        };
        c.place(i, add(start, offset)?, extent)?;
    }
    c.finish(minimum, preferred)
}
