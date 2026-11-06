//! A small QR encoder: byte mode, error-correction level M, versions 1 to 15.
//! It exists so the results can leave the board as a photograph, so it is
//! deliberately minimal: no structured append, no other EC levels, no ECI.
//!
//! The payload is short ASCII (`name=ok` per line), so a low version at a large
//! integer module scale is the readable choice. The caller draws the matrix; this
//! module only decides the modules.
const MAX_VERSION: usize = 15;
const MAX_MODULES: usize = 4 * MAX_VERSION + 17;

/// Error-correction level M: codewords per block, then up to two block groups
/// as (blocks, data codewords per block).
const LEVEL_M: [(usize, [(usize, usize); 2]); MAX_VERSION] = [(10, [(1, 16), (0, 0)]),
                                                              (16, [(1, 28), (0, 0)]),
                                                              (26, [(1, 44), (0, 0)]),
                                                              (18, [(2, 32), (0, 0)]),
                                                              (24, [(2, 43), (0, 0)]),
                                                              (16, [(4, 27), (0, 0)]),
                                                              (18, [(4, 31), (0, 0)]),
                                                              (22, [(2, 38), (2, 39)]),
                                                              (22, [(3, 36), (2, 37)]),
                                                              (26, [(4, 43), (1, 44)]),
                                                              (30, [(1, 50), (4, 51)]),
                                                              (22, [(6, 36), (2, 37)]),
                                                              (22, [(8, 37), (1, 38)]),
                                                              (24, [(4, 40), (5, 41)]),
                                                              (24, [(5, 41), (5, 42)])];

/// Alignment-pattern centres by version; version 1 has none.
const ALIGNMENT: [&[usize]; MAX_VERSION + 1] = [&[],
                                                &[],
                                                &[6, 18],
                                                &[6, 22],
                                                &[6, 26],
                                                &[6, 30],
                                                &[6, 34],
                                                &[6, 22, 38],
                                                &[6, 24, 42],
                                                &[6, 26, 46],
                                                &[6, 28, 50],
                                                &[6, 30, 54],
                                                &[6, 32, 58],
                                                &[6, 34, 62],
                                                &[6, 26, 46, 66],
                                                &[6, 26, 48, 70]];

const FORMAT_EC_M: u32 = 0b00;

const fn gf_tables() -> ([u8; 512], [u8; 256])
{
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut value = 1u16;
    let mut index = 0;
    while index < 255
    {
        exp[index] = value as u8;
        log[value as usize] = index as u8;
        value <<= 1;
        if value & 0x100 != 0
        {
            value ^= 0x11d;
        }
        index += 1;
    }
    while index < 512
    {
        exp[index] = exp[index - 255];
        index += 1;
    }
    (exp, log)
}

static TABLES: ([u8; 512], [u8; 256]) = gf_tables();

fn multiply(a: u8, b: u8) -> u8
{
    if a == 0 || b == 0
    {
        return 0;
    }
    let index = TABLES.1[a as usize] as usize + TABLES.1[b as usize] as usize;
    TABLES.0[index]
}

/// The generator polynomial for `degree` error-correction codewords, laid out
/// the way the shift register below consumes it (leading one implicit).
fn divisor(degree: usize) -> [u8; 256]
{
    let mut result = [0u8; 256];
    result[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree
    {
        for index in 0..degree
        {
            result[index] = multiply(result[index], root);
            if index + 1 < degree
            {
                result[index] ^= result[index + 1];
            }
        }
        root = multiply(root, 0x02);
    }
    result
}

fn remainder(data: &[u8], divisor: &[u8], degree: usize) -> [u8; 256]
{
    let mut result = [0u8; 256];
    for byte in data
    {
        let factor = byte ^ result[0];
        for index in 0..degree - 1
        {
            result[index] = result[index + 1];
        }
        result[degree - 1] = 0;
        for index in 0..degree
        {
            result[index] ^= multiply(divisor[index], factor);
        }
    }
    result
}

fn bch_format(data: u32) -> u32
{
    let mut rem = data;
    for _ in 0..10
    {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    (data << 10 | rem) ^ 0x5412
}

fn bch_version(version: u32) -> u32
{
    let mut rem = version;
    for _ in 0..12
    {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1f25);
    }
    version << 12 | rem
}

pub struct Qr
{
    pub size: usize,
    modules:  [[u8; MAX_MODULES]; MAX_MODULES],
    function: [[u8; MAX_MODULES]; MAX_MODULES]
}

impl Qr
{
    fn new(version: usize) -> Self
    {
        Self { size:     version * 4 + 17,
               modules:  [[0; MAX_MODULES]; MAX_MODULES],
               function: [[0; MAX_MODULES]; MAX_MODULES] }
    }

    pub fn dark(&self, x: usize, y: usize) -> bool
    {
        self.modules[y][x] != 0
    }

    fn set(&mut self, x: usize, y: usize, dark: bool)
    {
        self.modules[y][x] = u8::from(dark);
        self.function[y][x] = 1;
    }

    fn set_data(&mut self, x: usize, y: usize, dark: bool)
    {
        self.modules[y][x] = u8::from(dark);
    }

    fn draw_finder(&mut self, left: usize, top: usize)
    {
        let size = self.size as isize;
        for dy in -1..=7
        {
            for dx in -1..=7
            {
                let x = left as isize + dx;
                let y = top as isize + dy;
                if x < 0 || y < 0 || x >= size || y >= size
                {
                    continue;
                }
                let inside = (0..=6).contains(&dx) && (0..=6).contains(&dy);
                let dark = inside
                           && (dx == 0
                               || dx == 6
                               || dy == 0
                               || dy == 6
                               || ((2..=4).contains(&dx) && (2..=4).contains(&dy)));
                self.set(x as usize, y as usize, dark);
            }
        }
    }

    fn draw_alignment(&mut self, cx: usize, cy: usize)
    {
        for dy in -2isize..=2
        {
            for dx in -2isize..=2
            {
                let dark = dx.abs() == 2 || dy.abs() == 2 || (dx == 0 && dy == 0);
                self.set((cx as isize + dx) as usize, (cy as isize + dy) as usize, dark);
            }
        }
    }

    fn reserve_format(&mut self)
    {
        let size = self.size;
        for index in 0..6
        {
            self.set(8, index, false);
        }
        self.set(8, 7, false);
        self.set(8, 8, false);
        self.set(7, 8, false);
        for index in 0..6
        {
            self.set(index, 8, false);
        }
        for index in 0..8
        {
            self.set(size - 1 - index, 8, false);
        }
        for index in 0..7
        {
            self.set(8, size - 7 + index, false);
        }
        // The dark module is always set and is not a format bit.
        self.set(8, size - 8, true);
    }

    fn reserve_version(&mut self, version: usize)
    {
        if version < 7
        {
            return;
        }
        let size = self.size;
        for index in 0..18
        {
            let x = index / 3;
            let y = size - 11 + index % 3;
            self.set(x, y, false);
            self.set(size - 11 + index % 3, index / 3, false);
        }
    }

    fn place_data(&mut self, codewords: &[u8])
    {
        let size = self.size;
        let total_bits = codewords.len() * 8;
        let mut bit = 0usize;
        let mut upward = true;
        let mut right = size as isize - 1;
        while right > 0
        {
            if right == 6
            {
                right -= 1;
            }
            for step in 0..size
            {
                let y = if upward { size - 1 - step } else { step };
                for offset in 0..2
                {
                    let x = (right - offset) as usize;
                    if self.function[y][x] != 0
                    {
                        continue;
                    }
                    let dark = if bit < total_bits
                    {
                        codewords[bit / 8] >> (7 - bit % 8) & 1 != 0
                    }
                    else
                    {
                        false
                    };
                    self.set_data(x, y, dark);
                    bit += 1;
                }
            }
            upward = !upward;
            right -= 2;
        }
    }

    fn mask_bit(mask: usize, x: usize, y: usize) -> bool
    {
        match mask
        {
            0 => (x + y) % 2 == 0,
            1 => y % 2 == 0,
            2 => x % 3 == 0,
            3 => (x + y) % 3 == 0,
            4 => (y / 2 + x / 3) % 2 == 0,
            5 => (x * y) % 2 + (x * y) % 3 == 0,
            6 => ((x * y) % 2 + (x * y) % 3) % 2 == 0,
            _ => ((x + y) % 2 + (x * y) % 3) % 2 == 0
        }
    }

    fn apply_mask(&mut self, mask: usize)
    {
        for y in 0..self.size
        {
            for x in 0..self.size
            {
                if self.function[y][x] == 0 && Self::mask_bit(mask, x, y)
                {
                    self.modules[y][x] ^= 1;
                }
            }
        }
    }

    fn write_format(&mut self, mask: usize)
    {
        let bits = bch_format(FORMAT_EC_M << 3 | mask as u32);
        let size = self.size;
        let bit = |index: usize| bits >> index as u32 & 1 != 0;
        for index in 0..6
        {
            self.modules[index as usize][8] = u8::from(bit(index));
        }
        self.modules[7][8] = u8::from(bit(6));
        self.modules[8][8] = u8::from(bit(7));
        self.modules[8][7] = u8::from(bit(8));
        for index in 9..15
        {
            self.modules[8][(14 - index) as usize] = u8::from(bit(index));
        }
        for index in 0..8
        {
            self.modules[8][size - 1 - index] = u8::from(bit(index));
        }
        for index in 8..15
        {
            self.modules[size - 15 + index][8] = u8::from(bit(index));
        }
        self.modules[size - 8][8] = 1;
    }

    fn write_version(&mut self, version: usize)
    {
        if version < 7
        {
            return;
        }
        let bits = bch_version(version as u32);
        let size = self.size;
        for index in 0..18
        {
            let dark = bits >> index & 1 != 0;
            self.modules[size - 11 + index % 3][index / 3] = u8::from(dark);
            self.modules[index / 3][size - 11 + index % 3] = u8::from(dark);
        }
    }

    fn penalty(&self) -> u32
    {
        let size = self.size;
        let mut score = 0u32;
        let line = |values: &[u8]| -> u32 {
            let mut penalty = 0;
            let mut run = 1;
            for index in 1..values.len()
            {
                if values[index] == values[index - 1]
                {
                    run += 1;
                }
                else
                {
                    if run >= 5
                    {
                        penalty += 3 + (run - 5) as u32;
                    }
                    run = 1;
                }
            }
            if run >= 5
            {
                penalty += 3 + (run - 5) as u32;
            }
            // Finder-like 1:1:3:1:1 runs with four light modules beside them.
            let mut window = 0u32;
            for index in 0..values.len()
            {
                window = (window << 1 | u32::from(values[index])) & 0x7ff;
                if index >= 10 && (window == 0x05d || window == 0x5d0)
                {
                    penalty += 40;
                }
            }
            penalty
        };
        for y in 0..size
        {
            let mut row = [0u8; MAX_MODULES];
            row[..size].copy_from_slice(&self.modules[y][..size]);
            score += line(&row[..size]);
        }
        for x in 0..size
        {
            let mut column = [0u8; MAX_MODULES];
            for y in 0..size
            {
                column[y] = self.modules[y][x];
            }
            score += line(&column[..size]);
        }
        for y in 0..size - 1
        {
            for x in 0..size - 1
            {
                let value = self.modules[y][x];
                if value == self.modules[y][x + 1]
                   && value == self.modules[y + 1][x]
                   && value == self.modules[y + 1][x + 1]
                {
                    score += 3;
                }
            }
        }
        let mut dark = 0u32;
        for y in 0..size
        {
            for x in 0..size
            {
                dark += u32::from(self.modules[y][x]);
            }
        }
        let total = (size * size) as u32;
        let deviation = (dark as i64 * 20 - total as i64 * 10).unsigned_abs() as u32;
        let k = (deviation + total - 1) / total;
        score + k * 10
    }
}

/// Encode `payload` at the smallest version that fits, choosing the mask with
/// the lowest penalty. `None` means the payload does not fit version 15-M.
pub fn encode(payload: &[u8]) -> Option<Qr>
{
    let mut chosen = None;
    for version in 1..=MAX_VERSION
    {
        let (ec, groups) = LEVEL_M[version - 1];
        let data_words: usize = groups.iter().map(|(blocks, words)| blocks * words).sum();
        let capacity = data_words - if version <= 9 { 2 } else { 3 };
        if payload.len() <= capacity
        {
            chosen = Some((version, ec, groups, data_words));
            break;
        }
    }
    let (version, ec, groups, data_words) = chosen?;

    // Data codewords: mode, character count, payload, terminator and padding.
    let mut data = [0u8; 512];
    let mut bit = 0usize;
    let push = |value: u32, count: usize, data: &mut [u8; 512], bit: &mut usize| {
        for index in (0..count).rev()
        {
            if value >> index & 1 != 0
            {
                data[*bit / 8] |= 1 << (7 - *bit % 8);
            }
            *bit += 1;
        }
    };
    push(0b0100, 4, &mut data, &mut bit);
    push(payload.len() as u32, if version <= 9 { 8 } else { 16 }, &mut data, &mut bit);
    for &byte in payload
    {
        push(u32::from(byte), 8, &mut data, &mut bit);
    }
    let capacity_bits = data_words * 8;
    let terminator = (capacity_bits - bit).min(4);
    push(0, terminator, &mut data, &mut bit);
    while bit % 8 != 0
    {
        push(0, 1, &mut data, &mut bit);
    }
    let mut pad = 0xec;
    while bit < capacity_bits
    {
        push(pad, 8, &mut data, &mut bit);
        pad ^= 0xec ^ 0x11;
    }

    // Split into blocks, compute each block's error correction, interleave.
    let generator = divisor(ec);
    let mut blocks: [[u8; 256]; 16] = [[0; 256]; 16];
    let mut lengths = [0usize; 16];
    let mut corrections: [[u8; 256]; 16] = [[0; 256]; 16];
    let mut at = 0usize;
    let mut block = 0usize;
    for (count, words) in groups
    {
        for _ in 0..count
        {
            blocks[block][..words].copy_from_slice(&data[at..at + words]);
            lengths[block] = words;
            corrections[block] = remainder(&blocks[block][..words], &generator, ec);
            at += words;
            block += 1;
        }
    }
    let mut codewords = [0u8; 512];
    let mut out = 0usize;
    let longest = *lengths[..block].iter().max().unwrap_or(&0);
    for index in 0..longest
    {
        for block in 0..block
        {
            if index < lengths[block]
            {
                codewords[out] = blocks[block][index];
                out += 1;
            }
        }
    }
    for index in 0..ec
    {
        for block in 0..block
        {
            codewords[out] = corrections[block][index];
            out += 1;
        }
    }

    let mut qr = Qr::new(version);
    qr.draw_finder(0, 0);
    qr.draw_finder(qr.size - 7, 0);
    qr.draw_finder(0, qr.size - 7);
    for index in 8..qr.size - 8
    {
        qr.set(index, 6, index % 2 == 0);
        qr.set(6, index, index % 2 == 0);
    }
    let centers = ALIGNMENT[version];
    for &cy in centers
    {
        for &cx in centers
        {
            let finder = (cx == 6 && cy == 6)
                         || (cx == 6 && cy == qr.size - 7)
                         || (cx == qr.size - 7 && cy == 6);
            if !finder
            {
                qr.draw_alignment(cx, cy);
            }
        }
    }
    qr.reserve_format();
    qr.reserve_version(version);
    qr.place_data(&codewords[..out]);

    let mut best: Option<Qr> = None;
    let mut best_score = u32::MAX;
    for mask in 0..8
    {
        let mut trial = qr.clone();
        trial.apply_mask(mask);
        trial.write_format(mask);
        trial.write_version(version);
        let score = trial.penalty();
        if score < best_score
        {
            best_score = score;
            best = Some(trial);
        }
    }
    best
}

impl Clone for Qr
{
    fn clone(&self) -> Self
    {
        *self
    }
}

impl Copy for Qr
{
}
