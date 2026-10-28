//! Change one private warning string, preserving every other System resource.
use std::{env, fs, path::Path};

use mac_formats::{macbinary, resource};
const PANIC_LABELS: i16 = -16400;
fn main() -> mac_build::Result<()>
{
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 3
    {
        return Err(mac_build::error("expected FIXTURE_ELF OUTPUT_DIR BASE_SYSTEM_BIN"));
    }
    let output = Path::new(&args[1]);
    mac_build::package::build(&Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
                              Path::new(&args[0]),
                              output)?;
    let mut file = macbinary::decode(&fs::read(&args[2])?)?;
    let original = resource::decode(&file.resources)?;
    let mut resources = original.clone();
    let labels =
        resources.iter_mut()
                 .find(|r| r.kind == *b"STR#" && r.id == PANIC_LABELS)
                 .ok_or_else(|| mac_build::error("current System lacks panic STR# -16400"))?;
    let count =
        u16::from_be_bytes(labels.data
                                 .get(..2)
                                 .ok_or_else(|| mac_build::error("truncated panic STR# count"))?
                                 .try_into()?);
    if count != 22
    {
        return Err(mac_build::error("panic label contract changed: expected exactly 22 entries"));
    }
    let mut offset = 2;
    let mut replacement = None;
    for index in 0..count
    {
        let length =
            usize::from(*labels.data
                               .get(offset)
                               .ok_or_else(|| mac_build::error("truncated panic STR# length"))?);
        let end = offset + 1 + length;
        let text = labels.data
                         .get(offset + 1..end)
                         .ok_or_else(|| mac_build::error("truncated panic STR# value"))?;
        if index == 9
        {
            if text != b"Report truncated"
            {
                return Err(mac_build::error("panic label index 9 is not the TRUNCATED warning"));
            }
            replacement = Some(offset..end);
        }
        offset = end;
    }
    if offset != labels.data.len()
    {
        return Err(mac_build::error("panic STR# has trailing bytes"));
    }
    let replacement =
        replacement.ok_or_else(|| mac_build::error("panic TRUNCATED warning is absent"))?;
    let warning = mac_formats::text::pstring(&"é".repeat(255))?;
    if warning.len() != 256 || warning[0] != 255 || !warning[1..].iter().all(|&byte| byte == 0x8e)
    {
        return Err(mac_build::error("test warning must be 255 high MacRoman bytes"));
    }
    labels.data.splice(replacement, warning);
    file.resources = resource::encode_native(&resources)?;
    let decoded = resource::decode(&file.resources)?;
    if decoded.len() != original.len()
    {
        return Err(mac_build::error("private mutation changed resource count"));
    }
    let mut preserved = 0;
    for (before, after) in original.iter().zip(&decoded)
    {
        if before.kind == *b"STR#" && before.id == PANIC_LABELS
        {
            if before.kind != after.kind
               || before.id != after.id
               || before.name != after.name
               || before.attributes != after.attributes
               || before.data == after.data
            {
                return Err(mac_build::error("private warning mutation changed the wrong \
                                             metadata"));
            }
        }
        else
        {
            if before != after
            {
                return Err(mac_build::error("private warning mutation changed another System \
                                             resource"));
            }
            preserved += 1;
        }
    }
    fs::write(output.join("System.bin"), macbinary::encode(&file)?)?;
    fs::write(output.join("mutation.json"),
              serde_json::to_vec_pretty(&serde_json::json!({
                                            "resource_type": "STR#", "resource_id": PANIC_LABELS, "index": 9,
                                            "original_warning": "Report truncated", "replacement_macroman_bytes": 255,
                                            "replacement_utf8_bytes": 510, "other_resources_unchanged": preserved,
                                            "system_resource_count": original.len()
                                        }))?)?;
    Ok(())
}
