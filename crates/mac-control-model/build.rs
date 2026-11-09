//! Fingerprint the one property contract, including enum codecs and validation.
//! No older private format or fallback decoder is retained.
use std::{env, fs, path::PathBuf};
fn main()
{
    let mut hash = 0x811c9dc5u32;
    for path in ["src/messages.rs", "src/inventory.rs", "src/models.rs", "src/types.rs",
                 "src/codec.rs", "src/lib.rs"]
    {
        println!("cargo:rerun-if-changed={path}");
        for byte in fs::read(path).expect("control property schema source invariant")
        {
            hash = (hash ^ u32::from(byte)).wrapping_mul(0x01000193)
        }
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory invariant"));
    fs::write(out.join("fingerprint.rs"),format!("pub const SCHEMA_FINGERPRINT:u32=0x{hash:08x};\n")).expect("control schema fingerprint output invariant");
}
