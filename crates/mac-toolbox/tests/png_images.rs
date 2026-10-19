//! Aggregate public API tests against resident PNG services. Fixtures are encoded
//! independently with Python struct/zlib, not by the production decoder.
#![cfg(not(target_arch = "riscv64"))]
mod support;
// This host target exercises the shared PNG subset; run_all is covered by the
// aggregate acceptance target and the real graphical ECALL runner.
#[allow(dead_code)]
mod conformance;
use mac_toolbox::{BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED,
                  quickdraw::{Rect,
                              images::{ImageError, errors::*}}};
const ORACLE: &str = include_str!("data/png/oracle.json");
const ALPHA: &[u8] = include_bytes!("data/png/alpha-composite.png");
fn file(name: &str) -> Vec<u8>
{
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/png").join(format!("{name}.png"))).unwrap()
}
#[test]
fn all_static_png_color_modes_depths_filters_transparency_and_split_idats_use_resident_service()
{
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let valid = oracle["valid"].as_array().unwrap();
    assert_eq!(valid.len(), conformance::png::CASES.len());
    for (row, compiled) in valid.iter().zip(conformance::png::CASES)
    {
        assert_eq!(row["name"], compiled.name);
        assert_eq!(file(compiled.name), compiled.data);
        let expected: Vec<u8> =
            row["rgba"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        assert_eq!(expected, compiled.rgba);
        assert_eq!(row["width"], compiled.width);
        assert_eq!(row["height"], compiled.height);
        assert_eq!(row["depth"], compiled.depth);
        assert_eq!(row["color"], compiled.color);
        assert_eq!(row["alpha"], compiled.alpha);
    }
    support::with_system(|system| assert_eq!(conformance::png::run(system).unwrap(), 23));
}

#[test]
fn png_straight_alpha_nearest_scaling_and_clip_are_exact()
{
    support::with_system(|system| {
        let mut pixels = [0x204060; 24];
        let images = system.quickdraw().images();
        images.draw_to_surface(ALPHA,
                               &mut pixels,
                               6,
                               4,
                               Rect::new(0, 0, 6, 4),
                               Rect::new(1, 1, 5, 3))
              .unwrap();
        // Independently calculated source-over values for the six known samples.
        let expected = [0xff0000, 0x10a030, 0x204060, 0x4a4954, 0x13263a, 0x214161];
        for y in 0..4
        {
            for x in 0..6
            {
                let color = if (1..5).contains(&x) && (1..3).contains(&y)
                {
                    expected[(y / 2) * 3 + x / 2]
                }
                else
                {
                    0x204060
                };
                assert_eq!(pixels[y * 6 + x], color, "{x},{y}");
            }
        }
        let mut clipped = [0x204060; 24];
        images.draw_to_surface(ALPHA,
                               &mut clipped,
                               6,
                               4,
                               Rect::new(-2, -1, 4, 3),
                               Rect::new(0, 0, 6, 4))
              .unwrap();
        assert_eq!(clipped[0], expected[1]);
        assert_eq!(clipped[1], expected[1]);
        assert_eq!(clipped[2], expected[2]);
        assert_eq!(clipped[6], expected[4]);
        assert_eq!(clipped[18], 0x204060);
    });
}
#[test]
fn malformed_or_unsupported_png_never_partially_draws_and_reports_precise_errors()
{
    let cases = [("adam7", NOT_SUPPORTED, UNSUPPORTED),
                 ("apng", NOT_SUPPORTED, UNSUPPORTED),
                 ("short-inflate", BAD_ARGUMENT, BAD_COMPRESSED_DATA),
                 ("long-inflate", BAD_ARGUMENT, BAD_COMPRESSED_DATA),
                 ("bad-filter", BAD_ARGUMENT, BAD_COMPRESSED_DATA),
                 ("bad-adler", BAD_ARGUMENT, BAD_COMPRESSED_DATA),
                 ("inflate-bomb", BAD_ARGUMENT, BAD_COMPRESSED_DATA),
                 ("palette-index-outside", BAD_ARGUMENT, INVALID_PALETTE)];
    support::with_system(|system| {
        let images = system.quickdraw().images();
        for (name, status, kind) in cases
        {
            let mut pixels = [0x123456; 6];
            assert_eq!(images.draw_to_surface(&file(name),
                                              &mut pixels,
                                              3,
                                              2,
                                              Rect::new(0, 0, 3, 2),
                                              Rect::new(0, 0, 3, 2)),
                       Err(ImageError { status,
                                        kind }),
                       "{name}");
            assert_eq!(pixels, [0x123456; 6]);
        }
        let mut bad_crc = ALPHA.to_vec();
        bad_crc[29] ^= 1;
        assert_eq!(images.info(&bad_crc),
                   Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   CRC_MISMATCH }));
        for end in 0..ALPHA.len()
        {
            assert!(images.info(&ALPHA[..end]).is_err(), "prefix{end}");
        }
        let huge = vec![0; MAX_INPUT_BYTES + 1];
        assert_eq!(images.info(&huge),
                   Err(ImageError { status: NO_MEMORY,
                                    kind:   LIMIT_EXCEEDED }));
        let mut pixels = [0xabcdef; 6];
        assert_eq!(images.draw_to_surface(ALPHA,
                                          &mut pixels,
                                          3,
                                          2,
                                          Rect::ZERO,
                                          Rect::new(0, 0, 3, 2)),
                   Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   INVALID_DESTINATION }));
        assert_eq!(pixels, [0xabcdef; 6]);
        assert_eq!(images.draw_to_surface(ALPHA,
                                          &mut pixels,
                                          4,
                                          2,
                                          Rect::new(0, 0, 3, 2),
                                          Rect::new(0, 0, 3, 2)),
                   Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   INVALID_DESTINATION }));
    });
}
fn crc(bytes: &[u8]) -> u32
{
    let mut crc = 0xffffffffu32;
    for b in bytes
    {
        crc ^= u32::from(*b);
        for _ in 0..8
        {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb88320 } else { crc >> 1 };
        }
    }
    !crc
}
#[test]
fn dimensions_ordering_and_chunk_sizes_are_checked_before_allocation()
{
    support::with_system(|system| {
        let images = system.quickdraw().images();
        for (w, h, status, kind) in [(0u32, 2u32, BAD_ARGUMENT, INVALID_HEADER),
                                     (1025, 2, NO_MEMORY, LIMIT_EXCEEDED),
                                     (1024, 257, NO_MEMORY, LIMIT_EXCEEDED),
                                     (u32::MAX, 2, BAD_ARGUMENT, INVALID_HEADER)]
        {
            let mut input = ALPHA.to_vec();
            input[16..20].copy_from_slice(&w.to_be_bytes());
            input[20..24].copy_from_slice(&h.to_be_bytes());
            let c = crc(&input[12..29]);
            input[29..33].copy_from_slice(&c.to_be_bytes());
            assert_eq!(images.info(&input),
                       Err(ImageError { status,
                                        kind }));
        }
        let mut duplicate = ALPHA[..33].to_vec();
        duplicate.extend_from_slice(&ALPHA[8..33]);
        duplicate.extend_from_slice(&ALPHA[33..]);
        assert_eq!(images.info(&duplicate),
                   Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   INVALID_HEADER }));
        let mut oversized_chunk = ALPHA.to_vec();
        oversized_chunk[33..37].copy_from_slice(&0xffffffffu32.to_be_bytes());
        assert_eq!(images.info(&oversized_chunk),
                   Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   INVALID_CHUNK }));
    });
}
#[test]
fn mutated_compressed_payloads_are_bounded_and_do_not_panic_through_the_public_api()
{
    // Repair PNG CRC after each deterministic payload mutation so malformed
    // Huffman/zlib data reaches the bounded decoder, rather than failing CRC first.
    let start = 41;
    let n = u32::from_be_bytes(ALPHA[33..37].try_into().unwrap()) as usize;
    support::with_system(|system| {
        let images = system.quickdraw().images();
        for i in 0..n
        {
            for bit in [1, 8, 128]
            {
                let mut input = ALPHA.to_vec();
                input[start + i] ^= bit;
                let c = crc(&input[37..41 + n]);
                input[41 + n..45 + n].copy_from_slice(&c.to_be_bytes());
                let mut pixels = [0xabcdef; 6];
                let result = images.draw_to_surface(&input,
                                                    &mut pixels,
                                                    3,
                                                    2,
                                                    Rect::new(0, 0, 3, 2),
                                                    Rect::new(0, 0, 3, 2));
                if result.is_err()
                {
                    assert_eq!(pixels, [0xabcdef; 6]);
                }
            }
        }
    });
}
