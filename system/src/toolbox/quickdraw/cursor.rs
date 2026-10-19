//! Exact QuickDraw global arrow from the original Quadra 650 ROM.
//! See assets/cursors/provenance.json for its extraction record.
use mac_abi::quickdraw::MonoCursor;

pub fn arrow() -> MonoCursor
{
    // A built-in cursor is trusted data; the same checked decoder handles resources.
    super::cursors::decode_mono(include_bytes!("../../../assets/cursors/arrow.curs"))
        .expect("bundled ROM arrow")
}
