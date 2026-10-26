//! Metadata transport for host capture tools. mac-formats owns all parsing.
use std::{fs, path::Path};

use mac_build::{Result, error};
use mac_formats::resource::{Encoding,
                            index::{Index, MapLayout},
                            stream::Budget};
use serde_json::{Value, json};

pub fn inspect(path: &Path) -> Result<Value>
{
    let bytes = fs::read(path)?;
    let header = bytes.get(..16).ok_or_else(|| error("truncated resource header"))?;
    let layout = MapLayout::decode(header.try_into()?, bytes.len() as u64, Budget::default())?;
    let start = usize::try_from(layout.map_offset())?;
    let mut index = Index::decode(layout, bytes[start..][..layout.map_length()].to_vec())?;
    let mut entries = Vec::new();
    let mut number = 0;
    while let Some(entry) = index.entry(number)
    {
        let offset = usize::try_from(entry.offset)?;
        let end = offset.checked_add(4).ok_or_else(|| error("resource length offset overflow"))?;
        let length = u32::from_be_bytes(bytes.get(offset..end)
                                             .ok_or_else(|| error("truncated resource length"))?
                                             .try_into()?);
        index.record_length(number, length)?;
        let entry = index.entry(number).ok_or_else(|| error("validated resource index changed"))?;
        entries.push(json!({"kind":entry.kind,"id":entry.id,"attributes":entry.attributes,
            "name":entry.name,"reference_offset":entry.reference_offset,
            "payload_offset":entry.offset+4,"length":length}));
        number += 1;
    }
    let format = match index.encoding()
    {
        Encoding::Classic => "classic",
        Encoding::Native => "system_risc"
    };
    Ok(json!({"format":format,"resources":entries,
        "native_layout_fingerprint":mac_abi::NATIVE_LAYOUT_FINGERPRINT}))
}
