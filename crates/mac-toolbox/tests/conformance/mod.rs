//! Shared, no-std userspace conformance functions. The caller supplies a live
//! aggregate System; both host adapters and an RV64 ECALL runner can call these.
pub mod managed_controls;
pub mod worlds;
use mac_toolbox::{System,
                  quickdraw::{Point, Rect}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure
{
    Observation
    {
        case:  &'static str,
        field: &'static str
    },
    Service(i32),
    ServiceAt
    {
        case:   &'static str,
        status: i32
    },
    ControlPixel
    {
        case:     u16,
        x:        u16,
        y:        u16,
        actual:   u32,
        expected: u32
    },
    ControlHit
    {
        case:     u16,
        actual:   i16,
        expected: i16
    },
    Mismatch(&'static str)
}
impl From<mac_toolbox::Error> for Failure
{
    fn from(value: mac_toolbox::Error) -> Self
    {
        Self::Service(value.status())
    }
}
pub const RECT_A: &[u8] = include_bytes!("../data/regions/rect.bin");
pub const RECT_B: &[u8] = include_bytes!("../data/regions/rect_b.bin");
pub const INTERSECTION: &[u8] = include_bytes!("../data/regions/intersection.bin");
pub const UNION: &[u8] = include_bytes!("../data/regions/union.bin");
pub const DIFFERENCE: &[u8] = include_bytes!("../data/regions/difference.bin");
pub const XOR: &[u8] = include_bytes!("../data/regions/xor.bin");
pub const OVAL: &[u8] = include_bytes!("../data/regions/oval60x80.bin");
pub const INSET_OVAL: &[u8] = include_bytes!("../data/regions/inset_oval.bin");
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
/// Original basic region corpus plus public geometry/mapping/error contracts.
/// Expected region bytes are independent original-Mac capture fixtures.
pub fn geometry_and_regions(system: &System<'_>) -> Result<usize, Failure>
{
    let q = system.quickdraw();
    let a = Rect::new(20, 20, 160, 120);
    let b = Rect::new(70, 50, 210, 150);
    require(q.intersect(a, b)? == Some(Rect::new(70, 50, 160, 120)), "rectangle intersection")?;
    require(q.intersect(a, Rect::new(160, 20, 180, 120))?.is_none(), "touching rectangles")?;
    require(q.union(a, b)? == Rect::new(20, 20, 210, 150), "rectangle union")?;
    let source = Rect::new(0, 0, 10, 10);
    let destination = Rect::new(20, 30, 40, 60);
    require(q.map_point(Point { h: 5,
                                v: 5 },
                        source,
                        destination)?
            == Point { h: 30,
                       v: 45 },
            "point map")?;
    require(q.map_rect(Rect::new(2, 3, 8, 9), source, destination)? == Rect::new(24, 39, 36, 57),
            "rectangle map")?;
    require(q.scale_point(Point { h: -5,
                                  v: 7 },
                          Point { h: 3,
                                  v: 2 },
                          Point { h: 2,
                                  v: 3 })?
            == Point { h: -7,
                       v: 4 },
            "signed point scale")?;
    let mut polygon = [Point { h: 0,
                               v: 0 },
                       Point { h: 5,
                               v: 5 },
                       Point { h: 10,
                               v: 10 }];
    q.map_polygon(&mut polygon, source, destination)?;
    require(polygon
            == [Point { h: 20,
                        v: 30 },
                Point { h: 30,
                        v: 45 },
                Point { h: 40,
                        v: 60 }],
            "polygon map")?;
    require(q.map_point(Point::default(), Rect::ZERO, destination)
            == Err(mac_toolbox::Error::BadArgument),
            "empty mapping source")?;
    let regions = q.regions();
    let mut a_output = [0u8; 1024];
    let mut b_output = [0u8; 1024];
    let mut output = [0u8; 2048];
    let a = regions.rectangle(a, &mut a_output)?;
    let b = regions.rectangle(b, &mut b_output)?;
    require(a.bytes() == RECT_A, "original rectangle A")?;
    require(b.bytes() == RECT_B, "original rectangle B")?;
    require(regions.intersection(a, b, &mut output)?.bytes() == INTERSECTION,
            "original region intersection")?;
    require(regions.union(a, b, &mut output)?.bytes() == UNION, "original region union")?;
    require(regions.difference(a, b, &mut output)?.bytes() == DIFFERENCE,
            "original region difference")?;
    require(regions.xor(a, b, &mut output)?.bytes() == XOR, "original region xor")?;
    require(a.contains(Point { h: 20,
                               v: 20 })?
            && !a.contains(Point { h: 160,
                                   v: 20 })?
            && !a.contains(Point { h: 20,
                                   v: 120 })?,
            "half-open region boundary")?;
    let mut oval_output = [0u8; 2048];
    let oval = regions.oval(Rect::new(20, 340, 100, 400), &mut oval_output)?;
    require(oval.bytes() == OVAL, "original oval")?;
    let moved = regions.offset(oval,
                               Point { h: 150,
                                       v: 0 },
                               &mut output)?;
    let mut inset_output = [0u8; 2048];
    require(regions.inset(moved, 3, 2, &mut inset_output)?.bytes() == INSET_OVAL,
            "original inset oval")?;
    require(regions.parse(&[0; 9]).is_err_and(|e| e.status() == mac_abi::BAD_ARGUMENT),
            "malformed region")?;
    let mut short = [0xa5; 9];
    require(regions.rectangle(Rect::new(0, 0, 10, 10), &mut short)
                   .is_err_and(|e| e.status() == mac_abi::NO_MEMORY),
            "undersized region output")?;
    require(short == [0xa5; 9], "failed output untouched")?;
    Ok(20)
}
/// Reusable original oval corpus case. Scratch storage stays caller owned.
pub fn original_oval(system: &System<'_>, width: i16, height: i16, expected: &[u8],
                     output: &mut [u8])
                     -> Result<(), Failure>
{
    let actual = system.quickdraw().regions().oval(Rect::new(0, 0, width, height), output)?;
    require(actual.bytes() == expected, "original oval corpus bytes")
}

/// NewHandleClear0/1/16 observations from original System7.6.1 MemoryProbe.
/// This same function is suitable for native ECALL smoke execution.
pub fn memory_new_clear(system: &System<'_>) -> Result<usize, Failure>
{
    for size in [0, 1, 16]
    {
        let mut handle = system.memory().new_handle_clear(size)?;
        require(system.memory().last_error()?.is_none(), "new clear MemError")?;
        require(handle.len()? == size, "new clear size")?;
        require(handle.state()?.is_empty(), "new clear state")?;
        let mut output = [0xa5; 32];
        let count = handle.read(0, &mut output)?;
        require(count == size, "new clear read length")?;
        require(output[..size] == [0; 16][..size], "new clear original zero payload")?;
        require(output[size..].iter().all(|&b| b == 0xa5), "new clear output tail")?;
        handle.dispose()?;
    }
    Ok(3)
}

pub mod memory;
#[allow(dead_code)] // Ownership acceptance needs the real graphical Window Manager.
pub mod ownership;
pub use memory::memory_handles;
/// Run all267 original oval cases without a JSON parser or heap in userspace.
/// Compact records contain signedwidth/height plus originalbig-endianregionbytes.
pub fn original_oval_corpus(system: &System<'_>) -> Result<usize, Failure>
{
    let data = include_bytes!("../data/original-ovals.bin");
    let count = u16::from_be_bytes([data[0], data[1]]) as usize;
    let mut offset = 2;
    let mut output = [0u8; 8192];
    for _ in 0..count
    {
        let header =
            data.get(offset..offset + 6).ok_or(Failure::Mismatch("truncated oval fixture"))?;
        let w = i16::from_be_bytes([header[0], header[1]]);
        let h = i16::from_be_bytes([header[2], header[3]]);
        let n = u16::from_be_bytes([header[4], header[5]]) as usize;
        offset += 6;
        let bytes =
            data.get(offset..offset + n).ok_or(Failure::Mismatch("truncated oval bytes"))?;
        original_oval(system, w, h, bytes, &mut output)?;
        offset += n;
    }
    require(offset == data.len(), "trailing oval fixture bytes")?;
    Ok(count)
}

/// The aggregate userspace suite owns all acceptance decisions. Host and RV64
/// launchers only provide a System and propagate this result.
pub fn run_core(system: &System<'_>) -> Result<(), Failure>
{
    require(geometry_and_regions(system)? == 20, "geometry conformance count")?;
    require(original_oval_corpus(system)? == 267, "original oval conformance count")?;
    require(memory_new_clear(system)? == 3, "original clear allocation count")?;
    require(memory_handles(system)? == 48, "original handle observation count")?;
    require(scrap::original_scrap(system)? == 18, "original scrap observation count")?;
    require(scrap::native_scrap(system)? == 17, "native scrap contract count")?;
    Ok(())
}

/// Graphical profile requires real loaded definitions and resident resources.
pub fn run_all(system: &System<'_>) -> Result<(), Failure>
{
    worlds::run(system)?;
    events::run(system)?;
    rust_api::events_and_errors(system)?;
    textedit::original(system)?;
    textedit::native(system)?;
    ownership::editors(system, None)?;
    textedit::navigation(system)?;
    dates::values()?;
    run_core(system)?;
    require(png::run(system)? == 23, "PNG conformance count")?;
    require(controls::original_control_pixels(system)? == 54, "original control pixel case count")?;
    require(controls::original_control_hits(system)? == 8424, "original control hit record count")?;
    require(controls::original_scrollbar_pixels(system)? == 32,
            "original scrollbar pixel case count")?;
    controls::control_pixel_bounds(system)?;
    Ok(())
}

pub mod controls;

#[allow(dead_code)]
pub mod strings;
pub mod dates;

pub mod png;
pub mod events;
pub mod textedit;

#[allow(dead_code)] // Legacy gate checks are called only by string/guest runners.
pub mod scrap;

pub mod rust_api;
