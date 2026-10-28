//! Boot-admitted immutable Standard File resources. Input and paint borrow this
//! one checked copy instead of reparsing templates or allocating label strings.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{resources, standard_file::GET, *};

use super::{Sheet, layout::Layout};

struct Templates
{
    layouts: [Layout; 5],
    labels:  [String; 15],
    icons:   [crate::toolbox::quickdraw::icons_service::Drawing; 3]
}
static TEMPLATES: spin::Once<Templates> = spin::Once::new();

fn load(id: i16) -> Result<Layout, Status>
{
    crate::kernel::with_resources(|resources| {
        let (_, dialog) = resources.get(resources::SYSTEM,
                                        ResourceKey { kind: *b"DLOG",
                                                      id })?;
        let (_, items) = resources.get(resources::SYSTEM,
                                       ResourceKey { kind: *b"DITL",
                                                     id })?;
        Layout::parse(&dialog.data, &items.data)
    }).ok_or(NOT_SUPPORTED)?
}

/// Called during ordinary bootstrap after the immutable System resources are
/// installed. Failure leaves the facility unavailable; there is no late lazy
/// allocation path from a low-memory dialog request.
pub(crate) fn prepare() -> Result<(), Status>
{
    TEMPLATES.try_call_once(|| {
                 let layouts =
                     [load(-6042)?, load(-6043)?, load(-6044)?, load(-6045)?, load(-6046)?];
                 let mut labels = Vec::new();
                 labels.try_reserve_exact(15).map_err(|_| NO_MEMORY)?;
                 for index in 0..15
                 {
                     let mut bytes = [0; INLINE_STRING_BYTES];
                     let mut call = resources::ResourceCall { operation: resources::STRING,
                                                              scope: resources::SYSTEM,
                                                              key: ResourceKey { kind: *b"STR#",
                                                                                 id:   -16490 },
                                                              index,
                                                              ..Default::default() };
                     crate::kernel::with_resources(|resources| {
                         resources.request(&mut call, "", &mut bytes)
                     }).ok_or(NOT_SUPPORTED)??;
                     let text = core::str::from_utf8(bytes.get(..call.transferred)
                                                          .ok_or(BAD_ABI)?).map_err(|_| BAD_ABI)?;
                     let mut label = String::new();
                     label.try_reserve_exact(text.len()).map_err(|_| NO_MEMORY)?;
                     label.push_str(text);
                     labels.push(label);
                 }
                 let icon = |id| {
                     crate::kernel::with_resources(|resources| {
                         crate::toolbox::quickdraw::icons_service::Drawing::new(resources,
                &mac_abi::quickdraw::IconCall { scope: resources::SYSTEM, id,
                    destination: Rect::new(0,0,16,16), ..Default::default() },
                Default::default())
                     }).ok_or(NOT_SUPPORTED)?
                 };
                 let icons = [icon(-3999)?, icon(-4000)?, icon(-3996)?];
                 Ok::<_, Status>(Templates { layouts,
                                             icons,
                                             labels: labels.try_into().map_err(|_| BAD_ABI)? })
             })?;
    Ok(())
}

pub(in crate::toolbox) fn layout(mode: u32, sheet: Sheet) -> Result<&'static Layout, Status>
{
    let templates = TEMPLATES.get().ok_or(NOT_SUPPORTED)?;
    Ok(&templates.layouts[match sheet
                          {
                              Sheet::Main =>
                              {
                                  if mode == GET
                                  {
                                      0
                                  }
                                  else
                                  {
                                      1
                                  }
                              }
                              Sheet::Error => 2,
                              Sheet::Replace => 3,
                              Sheet::NewFolder => 4
                          }])
}

pub(in crate::toolbox) fn label(index: u32) -> Result<&'static str, Status>
{
    TEMPLATES.get()
             .ok_or(NOT_SUPPORTED)?
             .labels
             .get(index as usize)
             .map(String::as_str)
             .ok_or(BAD_ARGUMENT)
}

pub(in crate::toolbox) fn fallback(
    entry: &mac_abi::files::CatalogInfo)
    -> Result<crate::toolbox::quickdraw::icons_service::Drawing, Status>
{
    let index = if entry.is_directory()
    {
        0
    }
    else if matches!(&entry.file_type, b"APPL" | b"FNDR")
    {
        2
    }
    else
    {
        1
    };
    Ok(TEMPLATES.get().ok_or(NOT_SUPPORTED)?.icons[index].clone())
}

/// The managed viewport reserves committed plus staging pixels. This is derived
/// from the actual two shipped templates, not a second hand-maintained limit.
pub(in crate::toolbox) fn raster_budget() -> Result<usize, Status>
{
    let templates = TEMPLATES.get().ok_or(NOT_SUPPORTED)?;
    templates.layouts[..2].iter().try_fold(0usize, |largest, layout| {
                                     let list = layout.item(7);
                                     let width = usize::try_from(i32::from(list.right)
                                                                 - i32::from(list.left)
                                                                 - 17).map_err(|_| BAD_ABI)?;
                                     let height = usize::try_from(i32::from(list.bottom)
                                                                  - i32::from(list.top)
                                                                  - 2).map_err(|_| BAD_ABI)?;
                                     let bytes = width.checked_mul(height)
                                                      .and_then(|n| n.checked_mul(8))
                                                      .ok_or(BAD_ABI)?;
                                     Ok(largest.max(bytes))
                                 })
}

/// Main controls remain alive behind an auxiliary sheet. Count the actual
/// resource-defined buttons, including the Main template's New Folder user item.
pub(in crate::toolbox) fn required_control_count() -> Result<usize, Status>
{
    let templates = TEMPLATES.get().ok_or(NOT_SUPPORTED)?;
    let main = templates.layouts[..2].iter()
                                     .enumerate()
                                     .map(|(mode, layout)| {
                                         layout.items
                                               .iter()
                                               .enumerate()
                                               .filter(|(index, item)| {
                                                   item.kind & 0x7f == 4
                                                   || (mode == 1 && *index == 11)
                                               })
                                               .count()
                                     })
                                     .max()
                                     .ok_or(BAD_ABI)?;
    let auxiliary =
        templates.layouts[2..].iter()
                              .map(|layout| {
                                  layout.items.iter().filter(|item| item.kind & 0x7f == 4).count()
                              })
                              .max()
                              .ok_or(BAD_ABI)?;
    main.checked_add(auxiliary).ok_or(BAD_ABI)
}
