//! Nonzero-winding monochrome center sampling of hinted quadratic outlines.
use alloc::vec::Vec;

use ab_glyph_rasterizer::{Point, point};
use skrifa::outline::pen::PathElement;

fn midpoint(a: Point, b: Point) -> Point
{
    point((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}
fn quadratic(edges: &mut Vec<(Point, Point)>, a: Point, b: Point, c: Point, depth: u8)
{
    if depth >= 12 || ((a.x - 2.0 * b.x + c.x).abs() + (a.y - 2.0 * b.y + c.y).abs()) < 0.03125
    {
        edges.push((a, c));
        return;
    }
    let ab = midpoint(a, b);
    let bc = midpoint(b, c);
    let mid = midpoint(ab, bc);
    quadratic(edges, a, ab, mid, depth + 1);
    quadratic(edges, mid, bc, c, depth + 1);
}

pub(super) fn rasterize(path: &[PathElement], width: usize, height: usize,
                        transform: &impl Fn(f32, f32) -> Point)
                        -> Vec<bool>
{
    let mut edges = Vec::new();
    let mut previous = point(0.0, 0.0);
    let mut first = previous;
    for element in path
    {
        match *element
        {
            PathElement::MoveTo { x,
                                  y } =>
            {
                previous = transform(x, y);
                first = previous;
            }
            PathElement::LineTo { x,
                                  y } =>
            {
                let p = transform(x, y);
                edges.push((previous, p));
                previous = p;
            }
            PathElement::QuadTo { cx0,
                                  cy0,
                                  x,
                                  y } =>
            {
                let p = transform(x, y);
                quadratic(&mut edges, previous, transform(cx0, cy0), p, 0);
                previous = p;
            }
            PathElement::CurveTo { cx0,
                                   cy0,
                                   cx1,
                                   cy1,
                                   x,
                                   y } =>
            {
                let a = previous;
                let b = transform(cx0, cy0);
                let c = transform(cx1, cy1);
                let d = transform(x, y);
                for step in 1..=64
                {
                    let t = step as f32 / 64.0;
                    let u = 1.0 - t;
                    let p = point(u * u * u * a.x
                                  + 3.0 * u * u * t * b.x
                                  + 3.0 * u * t * t * c.x
                                  + t * t * t * d.x,
                                  u * u * u * a.y
                                  + 3.0 * u * u * t * b.y
                                  + 3.0 * u * t * t * c.y
                                  + t * t * t * d.y);
                    edges.push((previous, p));
                    previous = p;
                }
            }
            PathElement::Close =>
            {
                edges.push((previous, first));
                previous = first;
            }
        }
    }
    let mut coverage = alloc::vec![false;width*height];
    let mut crossings = Vec::new();
    for row in 0..height
    {
        crossings.clear();
        let y = row as f32 + 0.5;
        for &(a, b) in &edges
        {
            if (a.y <= y && b.y > y) || (b.y <= y && a.y > y)
            {
                crossings.push((a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y),
                                if b.y > a.y { 1i32 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0;
        let mut start = 0.0;
        for &(x, sign) in &crossings
        {
            let old = winding;
            winding += sign;
            if old == 0 && winding != 0
            {
                start = x;
            }
            if old != 0 && winding == 0
            {
                let left = (libm::ceilf(start - 0.5) as i32).clamp(0, width as i32) as usize;
                let right = (libm::ceilf(x - 0.5) as i32).clamp(0, width as i32) as usize;
                for column in left..right
                {
                    coverage[row * width + column] = true;
                }
            }
        }
    }
    coverage
}
