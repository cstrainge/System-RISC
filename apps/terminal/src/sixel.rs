//! Bounded DEC sixel rasters. Coordinates and repeats are clipped before painting.
pub const INPUT_LIMIT: usize = 256 * 1024;
const MAX_SIDE: usize = 1024;
const MAX_PIXELS: usize = 256 * 1024;

pub struct Raster
{
    pub width:  usize,
    pub height: usize,
    pub pixels: Vec<u32>
}

pub fn decode(mut bytes: &[u8], background: u32) -> Option<Raster>
{
    bytes = bytes.strip_prefix(b"\"")?;
    let [aspect_y, aspect_x, width, height] = parameters::<4>(&mut bytes)?;
    if aspect_y == 0
       || aspect_x == 0
       || aspect_y != aspect_x
       || width == 0
       || height == 0
       || width > MAX_SIDE
       || height > MAX_SIDE
       || width * height > MAX_PIXELS
    {
        return None;
    }

    let mut pixels = Vec::new();
    pixels.try_reserve_exact(width * height).ok()?;
    pixels.resize(width * height, background);
    let mut colors = std::array::from_fn::<_, 256, _>(|i| crate::color::indexed(i as u8));
    let (mut x, mut y, mut color) = (0usize, 0usize, 0usize);
    while let Some((&byte, rest)) = bytes.split_first()
    {
        bytes = rest;
        match byte
        {
            b'#' =>
            {
                color = number(&mut bytes)?.min(255);
                if let Some(rest) = bytes.strip_prefix(b";")
                {
                    bytes = rest;
                    let [space, r, g, b] = parameters::<4>(&mut bytes)?;
                    if space == 2 && r <= 100 && g <= 100 && b <= 100
                    {
                        let channel = |v| (v * 255 + 50) as u32 / 100;
                        colors[color] = (channel(r) << 16) | (channel(g) << 8) | channel(b);
                    }
                }
            }
            b'$' => x = 0,
            b'-' =>
            {
                x = 0;
                y = y.saturating_add(6);
            }
            b'!' | b'?'..=b'~' =>
            {
                let (repeat, dots) = if byte == b'!'
                {
                    let repeat = number(&mut bytes)?;
                    let (&dots, rest) = bytes.split_first()?;
                    bytes = rest;
                    if !(b'?'..=b'~').contains(&dots)
                    {
                        return None;
                    }
                    (repeat, dots - b'?')
                }
                else
                {
                    (1, byte - b'?')
                };
                let end = x.saturating_add(repeat);
                for bit in 0..6
                {
                    if dots & (1 << bit) != 0 && y + bit < height
                    {
                        let row = (y + bit) * width;
                        pixels[row + x.min(width)..row + end.min(width)].fill(colors[color]);
                    }
                }
                x = end;
            }
            b'\r' | b'\n' => (),
            _ => return None
        }
    }
    Some(Raster { width,
                  height,
                  pixels })
}

fn number(bytes: &mut &[u8]) -> Option<usize>
{
    let mut digits = 0;
    let mut value = 0usize;
    while let Some((&digit, rest)) = bytes.split_first()
    {
        if !digit.is_ascii_digit()
        {
            break;
        }
        value = value.checked_mul(10)?.checked_add(usize::from(digit - b'0'))?;
        *bytes = rest;
        digits += 1;
    }
    (digits != 0).then_some(value)
}

fn parameters<const N: usize>(bytes: &mut &[u8]) -> Option<[usize; N]>
{
    let mut result = [0; N];
    for (i, value) in result.iter_mut().enumerate()
    {
        if i != 0
        {
            *bytes = bytes.strip_prefix(b";")?;
        }
        *value = number(bytes)?;
    }
    Some(result)
}
