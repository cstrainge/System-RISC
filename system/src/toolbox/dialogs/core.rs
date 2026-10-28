//! Immutable, resource-backed templates for the guaranteed System alert set.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{dialogs::*, resources, *};

use super::{Alert, Item};

const IDS: [i16; 3] = [CORE_ERROR_ALERT, CORE_OUT_OF_MEMORY_ALERT, CORE_SAVE_CHANGES_ALERT];
static TEMPLATES: spin::Once<[Alert; 3]> = spin::Once::new();

pub(crate) fn supported(scope: resources::Scope, id: i16) -> bool
{
    scope == resources::SYSTEM && IDS.contains(&id)
}

pub(crate) fn required_control_count() -> Result<usize, Status>
{
    Ok(TEMPLATES.get()
                .ok_or(NOT_SUPPORTED)?
                .iter()
                .map(|alert| alert.items.iter().filter(|item| item.kind == 4).count())
                .max()
                .unwrap_or(0))
}

pub(crate) fn prepare() -> Result<(), Status>
{
    TEMPLATES.try_call_once(|| {
                 let load = |id| {
                     let mut call = DialogCall { scope: resources::SYSTEM,
                                                 resource: id,
                                                 text_length: 2,
                                                 ..Default::default() };
                     call.text[..2].copy_from_slice(b"^0");
                     crate::kernel::with_resources(|resources| super::load(resources, &call))
                .ok_or(NOT_SUPPORTED)?
                 };
                 Ok::<_, Status>([load(IDS[0])?, load(IDS[1])?, load(IDS[2])?])
             })?;
    Ok(())
}

fn substitute(template: &str, value: &str) -> Result<String, Status>
{
    let mut length = 0usize;
    for (at, part) in template.split("^0").enumerate()
    {
        if at != 0
        {
            length = length.checked_add(value.len()).ok_or(BAD_ARGUMENT)?;
        }
        length = length.checked_add(part.len()).ok_or(BAD_ARGUMENT)?;
    }
    if length > INLINE_STRING_BYTES
    {
        return Err(BAD_ARGUMENT);
    }
    let mut output = String::new();
    output.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    for (at, part) in template.split("^0").enumerate()
    {
        if at != 0
        {
            output.push_str(value);
        }
        output.push_str(part);
    }
    Ok(output)
}

pub(crate) fn instantiate(call: &DialogCall) -> Result<Alert, Status>
{
    if call.reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let index = IDS.iter().position(|&id| id == call.resource).ok_or(NOT_SUPPORTED)?;
    let template = &TEMPLATES.get().ok_or(NOT_SUPPORTED)?[index];
    let value = core::str::from_utf8(call.text
                                         .get(..call.text_length as usize)
                                         .ok_or(BAD_ARGUMENT)?).map_err(|_| BAD_ARGUMENT)?;
    let mut items = Vec::new();
    items.try_reserve_exact(template.items.len()).map_err(|_| NO_MEMORY)?;
    for item in &template.items
    {
        items.push(Item { bounds:  item.bounds,
                          kind:    item.kind,
                          enabled: item.enabled,
                          text:    substitute(&item.text, value)?,
                          icon:    item.icon });
    }
    Ok(Alert { bounds: template.bounds,
               position: template.position,
               items })
}
