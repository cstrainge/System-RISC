//! Every acceptance call enters aggregate SDK -> Client -> real resident CDEF.
#![cfg(not(target_arch = "riscv64"))]
mod support;
mod conformance;
#[test]
fn original_standard_control_pixels_match_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_control_pixels(system).unwrap(), 54)
    });
}
#[test]
fn original_control_hit_trace_matches_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_control_hits(system).unwrap(), 8424)
    });
}
#[test]
fn safe_control_pixel_borrows_validate_storage()
{
    support::with_system(|system| conformance::controls::control_pixel_bounds(system).unwrap());
}
#[test]
fn aggregate_core_acceptance_remains_live()
{
    support::with_system(|system| conformance::run_all(system).unwrap());
}

#[test]
fn original_scrollbar_pixels_match_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_scrollbar_pixels(system).unwrap(), 32)
    });
}
