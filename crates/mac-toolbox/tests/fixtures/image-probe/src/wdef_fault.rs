//! Private packager-only RX prefix. Original production WDEF segments and
//! relocations are preserved; no definition source is linked into this tool.
use mac_build::{Result, error};
use mac_formats::{code::{CodeRegistry, NoTranslator},
                  native::image,
                  resource::Resource};
use mac_toolbox::abi::{DefinitionCall,
                       definitions::{CALCULATE, DISPOSE, DRAW}};

pub(super) fn message(mode: &str) -> Option<i16>
{
    match mode
    {
        "window-definition-draw-fault" => Some(DRAW),
        "window-definition-calculate-fault" => Some(CALCULATE),
        "window-definition-dispose-fault" => Some(DISPOSE),
        _ => None
    }
}
fn align_up(value: u64, page: u64) -> Result<u64>
{
    if !page.is_power_of_two()
    {
        return Err(error("fault trampoline requires checked page geometry"));
    }
    Ok(value.checked_add(page - 1).ok_or_else(|| error("trampoline page rounding overflow"))?
       & !(page - 1))
}
pub(super) fn append(resource: &mut Resource, selected: i16) -> Result<String>
{
    if resource.kind != *b"WDEF" || resource.id != 0
    {
        return Err(error("trampoline requires actual WDEF0 resource"));
    }
    if !matches!(selected, DRAW | CALCULATE | DISPOSE)
    {
        return Err(error("unrecognized WDEF fault message"));
    }
    if core::mem::size_of::<usize>() != 8
    {
        return Err(error("WDEF trampoline packager requires LP64 layout"));
    }
    let registry = CodeRegistry::new();
    let mut native = registry.prepare(resource, 0, &mut NoTranslator)?;
    let original_descriptor = native.descriptor;
    let original_code = native.code.clone();
    let original_relocations = native.relocations.clone();
    let decoded = image::decode(&original_code, &original_descriptor)?;
    let count =
        decoded.segments.len().checked_add(1).ok_or_else(|| error("segment count overflow"))?;
    if count > image::MAX_SEGMENTS
    {
        return Err(error("WDEF has no segment slot for private trampoline"));
    }
    let page = mac_build::platform::current().memory()?.page_bytes();
    let offset =
        align_up(original_descriptor.load_limit
                                    .checked_sub(original_descriptor.load_base)
                                    .ok_or_else(|| error("WDEF compact extent underflow"))?,
                 page)?;
    let entry = original_descriptor.load_base
                                   .checked_add(offset)
                                   .ok_or_else(|| error("trampoline entry overflow"))?;
    let limit = entry.checked_add(page).ok_or_else(|| error("trampoline extent overflow"))?;
    let policy =
        mac_build::platform::current().link_range(mac_formats::native::Role::CodeResource)?;
    if original_descriptor.load_base != policy.start || limit > policy.end
    {
        return Err(error("private WDEF trampoline does not fit the actual code-resource link \
                          policy"));
    }
    let message_offset = u32::try_from(core::mem::offset_of!(DefinitionCall, message))?;
    if message_offset > 2047
       || !message_offset.is_multiple_of(2)
       || usize::try_from(message_offset)?.checked_add(2)
                                          .is_none_or(|end| {
                                              end > core::mem::size_of::<DefinitionCall>()
                                          })
    {
        return Err(error("DefinitionCall message is outside aligned LHU immediate range"));
    }
    let selected = u32::try_from(selected)?;
    if selected > 2047
    {
        return Err(error("WDEF selected message does not fit ADDI immediate"));
    }
    // PC-relative AUIPC/JALR tail jump: no absolute pointer or extra relocation.
    // t0/t1 are caller-saved. a0, sp and ra remain exactly as originally entered.
    let jump_pc = entry.checked_add(16).ok_or_else(|| error("trampoline jump PC overflow"))?;
    let delta = i128::from(original_descriptor.entry) - i128::from(jump_pc);
    let high = delta.checked_add(2048).ok_or_else(|| error("jump rounding overflow"))? >> 12;
    let low = delta - (high << 12);
    if !(-524288..=524287).contains(&high)
       || !(-2048..=2047).contains(&low)
       || i128::from(jump_pc) + (high << 12) + low != i128::from(original_descriptor.entry)
    {
        return Err(error("original WDEF entry is outside checked AUIPC/JALR reach"));
    }
    let high_bits = u32::try_from(high.rem_euclid(1 << 20))?;
    let low_bits = u32::try_from(low.rem_euclid(1 << 12))?;
    let branch = 8u32;
    if branch != 16 - 8 || branch & 1 != 0
    {
        return Err(error("trampoline BNE does not skip exactly EBREAK"));
    }
    let words = [(message_offset << 20) | (10 << 15) | (5 << 12) | (5 << 7) | 0x03, /* lhu t0,message(a0) */
                 (selected << 20) | (6 << 7) | 0x13, // addi t1,zero,message
                 (((branch >> 12) & 1) << 31)
                 | (((branch >> 5) & 0x3f) << 25)
                 | (6 << 20)
                 | (5 << 15)
                 | (1 << 12)
                 | (((branch >> 1) & 0xf) << 8)
                 | (((branch >> 11) & 1) << 7)
                 | 0x63, // bne t0,t1,+8
                 0x0010_0073,                        // ebreak only for selected real message
                 (high_bits << 12) | (5 << 7) | 0x17, // auipc t0,high
                 (low_bits << 20) | (5 << 15) | 0x67]; // jalr zero,t0,low
    let encoded_branch = (((words[2] >> 31) & 1) << 12)
                         | (((words[2] >> 25) & 0x3f) << 5)
                         | (((words[2] >> 8) & 0xf) << 1)
                         | (((words[2] >> 7) & 1) << 11);
    let encoded_high = i128::from(i32::from_le_bytes(words[4].to_le_bytes()) >> 12);
    let encoded_low = i128::from(i32::from_le_bytes(words[5].to_le_bytes()) >> 20);
    if words[0] >> 20 != message_offset
       || words[1] >> 20 != selected
       || encoded_branch != branch
       || i128::from(jump_pc) + (encoded_high << 12) + encoded_low
          != i128::from(original_descriptor.entry)
    {
        return Err(error("emitted trampoline fields do not reconstruct selected message or \
                          original entry"));
    }
    let initialized = u64::try_from(words.len()
                                         .checked_mul(4)
                                         .ok_or_else(|| error("trampoline byte size overflow"))?)?;
    if initialized > page || !entry.is_multiple_of(page) || original_descriptor.entry & 1 != 0
    {
        return Err(error("trampoline RX/page/entry geometry is invalid"));
    }
    native.descriptor.entry = entry;
    native.descriptor.load_limit = limit;
    native.code[6..8].copy_from_slice(&u16::try_from(count)?.to_be_bytes());
    native.code.extend_from_slice(&offset.to_be_bytes());
    native.code.extend_from_slice(&initialized.to_be_bytes());
    native.code.extend_from_slice(&page.to_be_bytes());
    native.code.extend_from_slice(&(image::READ | image::EXECUTE).to_be_bytes());
    for word in words
    {
        native.code.extend_from_slice(&word.to_le_bytes());
    }
    // Segment count/header changes; every original record, body and RLOC byte
    // stays exact. Existing offsets and relative code therefore stay original.
    if native.code.get(8..original_code.len()) != original_code.get(8..)
       || native.relocations != original_relocations
    {
        return Err(error("private trampoline changed original WDEF segments or relocations"));
    }
    image::LoadPlan::new(&native.code,
                         &native.descriptor,
                         Some(&native.relocations),
                         native.descriptor.load_base)?;
    let mut staged = resource.clone();
    staged.data = native.descriptor.encode()?.to_vec();
    staged.data.extend_from_slice(&native.code);
    staged.data.extend_from_slice(&native.relocations);
    let checked = registry.prepare(&staged, 0, &mut NoTranslator)?;
    if checked.code != native.code
       || checked.relocations != original_relocations
       || checked.descriptor != native.descriptor
    {
        return Err(error("final private WDEF envelope did not preserve staged bytes"));
    }
    *resource = staged;
    Ok(format!("{{\"kind\":\"WDEF\",\"message\":{selected},\"message_offset\":{message_offset},\"\
                original_entry\":{},\"entry\":{entry},\"page_bytes\":{page},\"old_limit\":{},\"\
                new_limit\":{limit},\"jump_pc\":{jump_pc},\"jump_delta\":{delta},\"high\":{high},\
                \"low\":{low},\"branch_bytes\":{branch},\"initialized_bytes\":{initialized},\"\
                original_segments_preserved\":true,\"relocations_preserved\":true,\"words\":\
                [{}]}}\n",
               original_descriptor.entry,
               original_descriptor.load_limit,
               words.iter().map(|word| format!("\"{word:08x}\"")).collect::<Vec<_>>().join(",")))
}
