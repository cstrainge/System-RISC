//! Public graphics admission preserves its destination on allocation refusal.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_system::testing::with_services;
use mac_toolbox::{Error, Point, Rect,
                  quickdraw::{SurfacePort, bitmaps::Bitmap, graphics::TransferMode}};
fn picture(body: &[u8]) -> Vec<u8>
{
    let mut bytes = vec![0, 0];
    bytes.extend_from_slice(&Rect::new(0, 0, 16, 16).to_be_bytes());
    bytes.extend_from_slice(&[0x11, 1]);
    bytes.extend_from_slice(body);
    bytes.push(0xff);
    let length = bytes.len() as u16;
    bytes[..2].copy_from_slice(&length.to_be_bytes());
    bytes
}
#[test]
fn picture_command_and_payload_refusals_leave_pixels_unchanged()
{
    let empty = picture(&[]); // An empty recording needs no backing allocation.
    let painted = picture(&[0x31, 0, 0, 0, 0, 0, 16, 0, 16]);
    let comment = picture(&[0xa1, 0, 1, 0, 7, 1, 2, 3, 4, 5, 6, 7]);
    // A classic polygon record: size, bounds, and three (v,h) points.
    let polygon =
        picture(&[0x71, 0, 22, 0, 0, 0, 0, 0, 16, 0, 16, 0, 0, 0, 0, 0, 0, 0, 16, 0, 16, 0, 0]);
    with_services(|services| {
        let background = 0x123456;
        let mut pixels = [background; 256];
        let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                             width:  16,
                                             height: 16,
                                             origin: Point::default(),
                                             clip:   Rect::new(0, 0, 16, 16) };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        let mut port = SurfacePort::new(connection);
        for (data, size) in [(&painted, None), (&comment, Some(7)), (&polygon, Some(12))]
        {
            let observed = Observe::begin(true, size);
            let result = port.draw_picture(data, Rect::new(0, 0, 16, 16));
            let allocations = observed.finish();
            assert_eq!(result, Err(Error::NoMemory));
            assert!(allocations > 0, "the selected admission must actually be refused");
            assert!(pixels.iter().all(|&pixel| pixel == background),
                    "a failed picture admission published partial destination pixels");
        }
        assert!(Error::NoMemory.to_string().contains("memory"));
        let observed = Observe::begin(true, None);
        let empty_result = port.draw_picture(&empty, Rect::new(0, 0, 16, 16));
        let allocations = observed.finish();
        assert_eq!(empty_result, Ok(()));
        assert_eq!(allocations, 0);
        assert_eq!(pixels, [background; 256]);
        assert!(port.draw_picture(&painted, Rect::new(0, 0, 16, 16)).is_ok());
        assert_eq!(pixels, [0; 256]);
        assert!(port.draw_picture(&comment, Rect::new(0, 0, 16, 16)).is_ok());
        assert_eq!(pixels, [0; 256]);
        assert_eq!(port.draw_picture(&painted[..painted.len() - 1], Rect::new(0, 0, 16, 16)),
                   Err(Error::BadArgument));
        assert_eq!(pixels, [0; 256]);
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}
#[test]
fn bitmap_pixel_and_palette_refusals_are_atomic_and_retryable()
{
    with_services(|services| {
        let background = 0x123456;
        let mut pixels = [background; 16];
        let bounds = Rect::new(0, 0, 4, 4);
        let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                             width:  4,
                                             height: 4,
                                             origin: Point::default(),
                                             clip:   bounds };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        let mut port = SurfacePort::new(connection);
        let mono = Bitmap::Mono { data: &[0xf0; 4],
                                  row_bytes: 1,
                                  bounds };
        let rgb = Bitmap::Rgb32 { pixels: &[0xabcdef; 16],
                                  row_pixels: 4,
                                  bounds };
        let indexed = Bitmap::Indexed { data: &[0; 4],
                                        row_bytes: 1,
                                        bounds,
                                        depth: 1,
                                        palette: &[0x654321, 0xabcdef] };
        // Refuse each owned pixel representation, the indexed palette and the
        // general admission refusal. Retained payloads have one owner.
        let empty = Bitmap::Mono { data: &[],
                                   row_bytes: 1,
                                   bounds };
        for (bitmap, size) in
            [(&mono, Some(4)), (&rgb, Some(64)), (&indexed, Some(8)), (&mono, None)]
        {
            let observed = Observe::begin(true, size);
            let result = port.copy_bits(bitmap, bounds, bounds, TransferMode::Copy, &[]);
            let allocations = observed.finish();
            assert_eq!(result, Err(Error::NoMemory));
            assert!(allocations > 0);
            assert_eq!(pixels, [background; 16]);
        }
        assert!(port.copy_bits(&rgb, bounds, bounds, TransferMode::Copy, &[]).is_ok());
        assert_eq!(pixels, [0xabcdef; 16]);
        assert!(port.copy_bits(&indexed, bounds, bounds, TransferMode::Copy, &[]).is_ok());
        assert_eq!(pixels, [0x654321; 16]);
        assert_eq!(port.copy_bits(&empty, bounds, bounds, TransferMode::Copy, &[]),
                   Err(Error::BadArgument));
        assert_eq!(pixels, [0x654321; 16]);
        assert!(Error::NoMemory.to_string().contains("memory"));
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}
