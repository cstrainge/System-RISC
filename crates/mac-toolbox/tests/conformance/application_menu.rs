//! Captured application-menu contracts through the public Process/Menu SDKs.
//! Original provenance: tests/data/application-menu/{original,provenance}.json.
use mac_toolbox::{System, quickdraw::Rect};

use super::{Failure, require};

const MENU: mac_toolbox::MenuId = mac_toolbox::MenuId(-16489);
const MAX: usize = 21;

/// Call after each launch, activation, hide, restore and exit transition. This
/// observes the running system rather than manufacturing a replacement menu.
pub fn snapshot(system: &System<'_>) -> Result<usize, Failure>
{
    let processes = system.processes();
    let count = processes.count()? as usize;
    require((1..=MAX).contains(&count), "application menu process bounds")?;
    let mut entries = [mac_toolbox::processes::ProcessInfo::default(); MAX];
    for (index, entry) in entries[..count].iter_mut().enumerate()
    {
        *entry = processes.at(index as u32)?;
    }
    let entries = &mut entries[..count];
    require(entries.iter().filter(|entry| entry.foreground == true).count() == 1,
            "one application-menu foreground checkmark")?;
    require(entries.iter().all(|entry| entry.foreground == false || entry.hidden == false),
            "foreground application is visible")?;
    // Captured ASCII names establish alphabetic ordering independently of launch
    // order. The native interface extends this to Unicode lowercase ordering.
    entries.sort_unstable_by(|left, right| {
               left.name()
                   .chars()
                   .flat_map(char::to_lowercase)
                   .cmp(right.name().chars().flat_map(char::to_lowercase))
                   .then_with(|| left.id.cmp(&right.id))
           });
    let menus = system.menus();
    let layout = menus.layout(MENU)?;
    require(layout.title == Rect::new(601, 0, 634, 20),
            "captured right-side application title bounds")?;
    require(layout.popup.top == 20 && layout.popup.right == 633,
            "captured application popup anchor")?;
    require(layout.popup.bottom - layout.popup.top == 64 + count as i16 * 18,
            "captured command and application row heights")?;
    require(layout.popup.left >= 1 && layout.popup.bottom <= 476,
            "application popup and shadow stay within screen")?;
    require(menus.item_count(MENU)? == count as u16 + 4,
            "application menu includes every process")?;
    let foreground = entries.iter().find(|entry| entry.foreground == true).unwrap();
    let others_visible =
        entries.iter().any(|entry| entry.id != foreground.id && entry.hidden == false);
    let any_hidden = entries.iter().any(|entry| entry.hidden == true);
    for (row, command, enabled) in
        [(1, *b"hcur", others_visible), (2, *b"hoth", others_visible), (3, *b"sall", any_hidden)]
    {
        let item = menus.item(MENU, row)?;
        require(item.command == u32::from_be_bytes(command)
                && item.enabled == enabled
                && !item.checked,
                "captured application menu command state")?;
    }
    let separator = menus.item(MENU, 4)?;
    require(!separator.enabled && !separator.checked,
            "application menu separator cannot be selected")?;
    for (index, entry) in entries.iter().enumerate()
    {
        let item = menus.item(MENU, index as u16 + 5)?;
        require(item.command == index as u32 + 5
                && item.enabled
                && item.checked == (entry.foreground == true),
                "alphabetic process row selector and captured foreground mark")?;
    }
    require(menus.set_item_enabled(MENU, 1, !others_visible)
            == Err(mac_toolbox::Error::NotSupported),
            "system application menu is protected from application writes")?;
    require(menus.item(MENU, 1)?.enabled == others_visible,
            "rejected menu mutation preserves command policy")?;
    // Bounds/count/checkmark/command observations above; native protection adds2.
    Ok(14 + count)
}
