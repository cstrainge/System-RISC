//! Shared drawing must not know which backend acquired the scanout, and a
//! device that pads rows must still get a correct panel.
use mac_boot_screen::{HAPPY,
                      backend::{Backend, Frame},
                      draw_panel};

const WIDTH: usize = 640;
const HEIGHT: usize = 480;
const PAD: usize = 16;

/// A backend whose device exists only as numbers, so the drawing path is
/// exercised without hardware.
struct Fake
{
    width:  usize,
    height: usize,
    stride: usize
}
impl Backend for Fake
{
    type Error = &'static str;

    fn acquire<'a>(&mut self, pixels: &'a mut [u32]) -> Result<Frame<'a>, Self::Error>
    {
        let rows = self.stride.checked_mul(self.height).ok_or("the mode overflows")?;
        if pixels.len() < rows
        {
            return Err("the reserved storage is too small for the mode");
        }
        Ok(Frame { width:  self.width,
                   height: self.height,
                   stride: self.stride,
                   pixels: &mut pixels[..rows] })
    }
}

#[test]
fn a_padded_pitch_draws_the_same_panel_and_leaves_the_padding_alone()
{
    let mut packed = vec![0u32; WIDTH * HEIGHT];
    {
        let mut backend = Fake { width:  WIDTH,
                                 height: HEIGHT,
                                 stride: WIDTH };
        let mut frame = backend.acquire(&mut packed).unwrap();
        draw_panel(&mut frame.raster().unwrap(), HAPPY);
    }
    let stride = WIDTH + PAD;
    let mut padded = vec![0xdead_beefu32; stride * HEIGHT];
    {
        let mut backend = Fake { width: WIDTH,
                                 height: HEIGHT,
                                 stride };
        let mut frame = backend.acquire(&mut padded).unwrap();
        assert_eq!((frame.width, frame.height, frame.stride), (WIDTH, HEIGHT, stride));
        draw_panel(&mut frame.raster().unwrap(), HAPPY);
    }
    for y in 0..HEIGHT
    {
        for x in 0..WIDTH
        {
            assert_eq!(padded[y * stride + x],
                       packed[y * WIDTH + x],
                       "visible pixel {x},{y} differs from the packed render");
        }
        for x in WIDTH..stride
        {
            assert_eq!(padded[y * stride + x], 0xdead_beef, "padding {x},{y} was written");
        }
    }
}

#[test]
fn a_frame_narrower_than_a_row_or_without_storage_is_refused()
{
    let mut pixels = [0u32; 64];
    let mut narrow = Frame { width:  8,
                             height: 8,
                             stride: 7,
                             pixels: &mut pixels[..] };
    assert!(narrow.raster().is_none(), "a pitch below the width cannot be drawn");
    let mut short = Frame { width:  8,
                            height: 8,
                            stride: 8,
                            pixels: &mut pixels[..32] };
    assert!(short.raster().is_none(), "rows past the end of the slice cannot be drawn");
    let mut backend = Fake { width:  WIDTH,
                             height: HEIGHT,
                             stride: WIDTH };
    let mut storage = [0u32; 100];
    assert_eq!(backend.acquire(&mut storage).err(),
               Some("the reserved storage is too small for the mode"));
}
