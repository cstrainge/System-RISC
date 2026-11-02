//! Deterministic one-axis grid policy. Two such calculations resolve columns
//! before width-dependent row measurement. No service calls or allocation.
use mac_definition::reflow::{AxisConstraint, Error, Fill, Result, grid::AxisCalculation};





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
                if j + 1 < item.start + item.span
                {
                    sum = add(sum, t.gap)?;
                }
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



/// Capped equal fill. Binary search needs at most 63 scans, normally 15 for a
/// native finite viewport; it avoids one redistribution pass per saturated track.
fn fill(c: &mut AxisCalculation<'_>, left: i64) -> Result<()>
{
    if left == 0
    {
        return Ok(());
    }
    let capacity = |t: mac_definition::reflow::grid::Track| {
        if t.bounds.fill() == Fill::Fill { (t.maximum - t.assigned).min(left) } else { 0 }
    };
    let mut low = 0;
    let mut high = left;
    while low < high
    {
        let mid = low + (high - low) / 2 + 1;
        let mut spent = 0i128;
        for i in 0..c.track_count()
        {
            spent += i128::from(capacity(c.track(i)?).min(mid));
        }
        if spent <= i128::from(left)
        {
            low = mid;
        }
        else
        {
            high = mid - 1;
        }
    }
    let mut remaining = left;
    for i in 0..c.track_count()
    {
        let t = c.track(i)?;
        let add = capacity(t).min(low);
        c.set_track(i, t.minimum, t.preferred, t.assigned + add)?;
        remaining -= add;
    }
    for i in 0..c.track_count()
    {
        if remaining == 0
        {
            break;
        }
        let t = c.track(i)?;
        if t.bounds.fill() == Fill::Fill && t.assigned < t.maximum
        {
            c.set_track(i, t.minimum, t.preferred, t.assigned + 1)?;
            remaining -= 1;
        }
    }
    Ok(())
}

pub fn calculate(c: &mut AxisCalculation<'_>) -> Result<()>
{
    for i in 0..c.track_count()
    {
        c.set_track(i, 0, 0, 0)?;
        c.set_maximum(i, i64::MAX)?;
    }
    requirements(c, false)?;
    requirements(c, true)?;
    let gaps = total(c, |t| t.gap)?;
    let basis = c.percent_basis().map(|v| v.saturating_sub(gaps));
    let mut manual = false;
    for i in 0..c.track_count()
    {
        let t = c.track(i)?;
        let maximum = match t.bounds.maximum()
        {
            Some(v) => v.resolve(basis)?.unwrap_or(i64::MAX),
            None => i64::MAX
        };
        let explicit_minimum = match t.bounds.minimum()
        {
            Some(v) => v.resolve(basis)?,
            None => None
        };
        let minimum = explicit_minimum.unwrap_or(t.minimum.min(maximum));
        if minimum > maximum
        {
            return Err(Error::InvalidRequirements);
        }
        let preferred = t.requested.unwrap_or(t.preferred).clamp(minimum, maximum);
        manual |= t.requested.is_some();
        c.set_track(i, minimum, preferred, 0)?;
        c.set_maximum(i, maximum)?;
    }
    let minimum = add(total(c, |t| t.minimum)?, gaps)?;
    let preferred = add(total(c, |t| t.preferred)?, gaps)?;
    let budget = match c.constraint()
    {
        AxisConstraint::Unbounded => preferred,
        AxisConstraint::AtMost(v) => preferred.min(i64::from(v.pixels())),
        AxisConstraint::Exact(v) => i64::from(v.pixels())
    };
    let gap_budget = gaps.min(budget);
    let mut prefix = 0;
    let mut previous = 0;
    for i in 0..c.track_count()
    {
        prefix = add(prefix, c.track(i)?.gap)?;
        let end = fraction(gap_budget, prefix, gaps)?;
        c.set_gap(i, end - previous)?;
        previous = end;
    }
    let budget = budget - gap_budget;
    if matches!(c.constraint(), AxisConstraint::Unbounded)
    {
        for i in 0..c.track_count()
        {
            let t = c.track(i)?;
            c.set_track(i, t.minimum, t.preferred, t.preferred)?;
        }
    }
    else if budget < minimum - gaps
    {
        // Explicit forced-size policy: insufficient room scales minima. Maxima
        // still hold; child controls degrade safely, including zero-area cells.
        grow(c, budget, |t| t.minimum)?;
    }
    else
    {
        for i in 0..c.track_count()
        {
            let t = c.track(i)?;
            c.set_track(i, t.minimum, t.preferred, t.minimum)?;
        }
        let left = grow(c, budget - (minimum - gaps), |t| {
            if manual || t.bounds.fill() == Fill::None
            {
                (t.preferred - t.assigned).max(0)
            }
            else
            {
                0
            }
        })?;
        fill(c, left)?;
    }
    let mut position = 0;
    for i in 0..c.track_count()
    {
        c.set_position(i, position)?;
        position = add(position, add(c.track(i)?.assigned, c.track(i)?.assigned_gap)?)?;
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
