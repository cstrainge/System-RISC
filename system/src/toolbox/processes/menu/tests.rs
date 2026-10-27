//! Prepared native cleanup presentation remains usable with new allocations refused.
use alloc::{string::ToString, vec::Vec};

use mac_abi::menus::MenuIcon;
use mac_formats::resource::Resource;

use super::*;
use crate::{testing::allocations::Observe,
            toolbox::{events::EventManager,
                      windows::{GroupFocus, WindowManager}}};

extern "C" fn refused_gate(_: u32, _: *mut (), _: usize) -> i64
{
    NO_MEMORY as i64
}

fn labels() -> [Resource; 1]
{
    let mut bytes = Vec::from([0, 3]);
    for label in ["Hide ", "Hide Others", "Show All"]
    {
        bytes.push(label.len() as u8);
        bytes.extend_from_slice(label.as_bytes());
    }
    [Resource { kind:       *b"STR#",
                id:         RESOURCE_ID,
                name:       None,
                attributes: 0,
                data:       bytes }]
}

fn application_menu() -> ApplicationMenu
{
    let resources = labels();
    ApplicationMenu::new(crate::test_mdef::standard_menu_definition,
                         crate::toolbox::quickdraw::surface_gate,
                         Resources::new(&resources, &[], &[])).unwrap()
}

#[test]
fn menu_scratch_admission_refusals_leave_constructor_retryable()
{
    let resources = labels();
    let construct = || {
        ApplicationMenu::new(crate::test_mdef::standard_menu_definition,
                             crate::toolbox::quickdraw::surface_gate,
                             Resources::new(&resources, &[], &[]))
    };
    let observe = Observe::begin(false, None);
    let admitted = construct();
    let admissions = observe.finish();
    assert!(admitted.is_ok());
    assert!(admissions > 0);
    drop(admitted);
    for ordinal in 1..=admissions
    {
        let observe = Observe::fail_at(ordinal);
        let refused = construct();
        let attempted = observe.finish();
        assert!(attempted >= ordinal);
        assert!(matches!(refused, Err(NO_MEMORY)), "constructor admission {ordinal}");
        assert!(construct().is_ok(), "failed scratch admission must leave reusable resources");
    }
}

#[test]
fn admitted_menu_refresh_needs_no_heap_and_retains_snapshot_after_sizing_refusal()
{
    let mut menu = application_menu();
    let entries = [Entry { id:     1,
                           name:   "Finder",
                           hidden: false,
                           icon:   MenuIcon::default() },
                   Entry { id:     2,
                           name:   "Text Edit",
                           hidden: false,
                           icon:   MenuIcon::default() }];
    let observe = Observe::begin(true, None);
    let refreshed = menu.refresh(&entries, 1);
    let allocations = observe.finish();
    assert_eq!(refreshed, Ok(()));
    assert_eq!(allocations, 0, "initial refresh must use its pre-admitted candidate owner");
    let previous = *menu.state;
    let actions = menu.actions;
    let candidate_owner = &*menu.scratch as *const MenuDefinitionState;
    menu.tracking = true;
    menu.highlighted = 5;
    // Exercise a real MDEF measurement whose service returns a typed refusal.
    // The definition must not publish even its already-written candidate fields.
    menu.gate = refused_gate;
    let observe = Observe::begin(true, None);
    let refused = menu.refresh(&entries, 2);
    let allocations = observe.finish();
    assert_eq!(refused, Err(NO_MEMORY));
    assert_eq!(allocations, 0);
    assert!(*menu.state == previous);
    assert_eq!(menu.actions, actions);
    assert_eq!(&*menu.scratch as *const MenuDefinitionState, candidate_owner);
    assert!(!menu.tracking, "a failed membership refresh must cancel the old row selection");
    assert_eq!(menu.highlighted, 0);
    menu.gate = crate::toolbox::quickdraw::surface_gate;
    let observe = Observe::begin(true, None);
    let retried = menu.refresh(&entries[..1], 1);
    let allocations = observe.finish();
    assert_eq!(retried, Ok(()));
    assert_eq!(allocations, 0);
    assert_eq!(menu.state.item_count, (FIXED_ROWS + 1) as u32);
    assert_eq!(menu.actions[FIXED_ROWS], Some(Action::Activate(1)));
    assert!(!menu.actions.contains(&Some(Action::Activate(2))));
    assert!(mac_traps::Error::from_status(NO_MEMORY).to_string().contains("memory"));
    let previous = *menu.state;
    let excess = [entries[0]; MAX_APPLICATIONS + 1];
    assert_eq!(menu.refresh(&excess, 1), Err(NO_MEMORY));
    assert!(*menu.state == previous, "existing presentation capacity must refuse, not truncate");
}

#[test]
fn cleanup_focus_reuses_window_admission_scratch_and_retries_geometry_after_refusal()
{
    let mut windows = WindowManager::new();
    let mut events = EventManager::new();
    let a =
        windows.new_window(Rect::new(20, 80, 300, 260), "Survivor A", 1, (40, 40), &mut events, 0)
               .unwrap();
    let b =
        windows.new_window(Rect::new(50, 110, 330, 290), "Survivor B", 1, (40, 40), &mut events, 0)
               .unwrap();
    let other =
        windows.new_window(Rect::new(80, 140, 360, 320), "Other", 2, (40, 40), &mut events, 0)
               .unwrap();
    let owner = windows.window(a).unwrap().owner;
    let observe = Observe::begin(true, None);
    windows.select_owner_group_focus(owner, GroupFocus::Window(a), &mut events, 1);
    let allocations = observe.finish();
    // Exposure-region scratch may refuse, but the selected plane is committed
    // with bounded damage and its existing ID/order backing stays available.
    assert!(allocations > 0, "overlap must exercise the fallible exposure path");
    assert_eq!(windows.active_window(), Some(a));
    assert_eq!(windows.front_window(), Some(a));
    assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), [other, b, a]);
    let observe = Observe::begin(true, None);
    windows.select_owner_group_focus(owner, GroupFocus::Window(a), &mut events, 2);
    let allocations = observe.finish();
    assert_eq!(allocations, 0, "no-change focus must not construct exposure geometry");
    windows.select_owner_group_focus(owner, GroupFocus::Window(b), &mut events, 3);
    assert_eq!(windows.active_window(), Some(b));
    assert_eq!(windows.front_window(), Some(b));
    assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), [other, a, b]);
    let observe = Observe::begin(true, None);
    windows.select_owner_group_focus(owner, GroupFocus::Desktop, &mut events, 4);
    let allocations = observe.finish();
    assert_eq!(allocations, 0, "desktop focus changes no window order or content geometry");
    assert_eq!(windows.active_window(), None);
    assert_eq!(windows.front_window(), Some(b));
}

fn hide_fixture(interleaved: bool) -> (WindowManager, EventManager, [mac_abi::WindowId; 4])
{
    let mut windows = WindowManager::new();
    let mut events = EventManager::new();
    let already_hidden = windows.new_window(Rect::new(350, 55, 450, 150),
                                            "Already hidden",
                                            3,
                                            (40, 40),
                                            &mut events,
                                            0)
                                .unwrap();
    windows.set_hidden(already_hidden, true);
    let descriptors = [(Rect::new(20, 60, 120, 160), "Other back", 2),
                       (Rect::new(50, 80, 250, 240), "Owner back", 1),
                       (Rect::new(100, 110, 300, 270), "Other front", 2),
                       (Rect::new(160, 60, 360, 300), "Owner front", 1)];
    let order = if interleaved { [0, 1, 2, 3] } else { [0, 2, 1, 3] };
    let ids = core::array::from_fn(|index| {
        let (bounds, name, owner) = descriptors[order[index]];
        windows.new_window(bounds, name, owner, (40, 40), &mut events, 0).unwrap()
    });
    for id in ids
    {
        windows.validate(id).unwrap();
    }
    while events.next_matching(u16::MAX).is_some()
    {}
    (windows, events, ids)
}

#[test]
fn hide_others_batch_preserves_foreground_plane_and_repeated_calls_need_no_work()
{
    let mut empty = WindowManager::new();
    let observe = Observe::begin(true, None);
    let changed = empty.hide_other_owners(123, |_| true);
    let allocations = observe.finish();
    assert!(!changed);
    assert_eq!(allocations, 0);

    let (mut windows, mut events, ids) = hide_fixture(false);
    let owner = windows.window(ids[3]).unwrap().owner;
    let other = windows.window(ids[0]).unwrap().owner;
    let focus = windows.active_window();
    let observe = Observe::begin(true, None);
    let changed = windows.hide_other_owners(owner, |_| true);
    let allocations = observe.finish();
    assert!(changed);
    assert_eq!(allocations, 0, "background planes expose no foreground content");
    assert_eq!(windows.active_window(), focus);
    assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), ids[2..]);
    assert_eq!(windows.front_for_owner(other), None);
    for id in ids
    {
        assert!(windows.next_update_for(id, &events, 1).is_none(), "hide queued unrelated content");
    }
    assert!(events.next_matching(u16::MAX).is_none(), "hiding other planes must not change focus");

    let observe = Observe::begin(true, None);
    let changed = windows.hide_other_owners(owner, |_| true);
    let allocations = observe.finish();
    assert!(!changed);
    assert_eq!(allocations, 0);
    assert_eq!(windows.active_window(), focus);
    windows.set_owner_hidden(other, false);
    assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), ids);
    assert_eq!(windows.active_window(), focus);
}

#[test]
fn hide_others_exposes_only_final_visible_windows_and_survives_scratch_refusal()
{
    let (mut reference, mut reference_events, reference_ids) = hide_fixture(true);
    let other = reference.window(reference_ids[0]).unwrap().owner;
    reference.set_owner_hidden(other, true);
    let expected = reference.begin_update(reference_ids[1]);
    assert!(expected.is_some());
    assert!(reference.next_update_for(reference_ids[3], &reference_events, 0).is_none());
    reference.end_update(reference_ids[1]);
    assert!(reference_events.next_matching(u16::MAX).is_none());

    for refuse in [false, true]
    {
        let (mut windows, mut events, ids) = hide_fixture(true);
        let owner = windows.window(ids[3]).unwrap().owner;
        let focus = windows.active_window();
        let observe = Observe::begin(refuse, None);
        let changed = windows.hide_other_owners(owner, |_| true);
        let allocations = observe.finish();
        assert!(changed);
        assert!(allocations > 0, "interleaved planes must exercise exposure geometry");
        assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), [ids[1], ids[3]]);
        assert_eq!(windows.active_window(), focus);
        assert!(windows.next_update_for(ids[3], &events, 0).is_none());
        assert!(windows.next_update_for(ids[0], &events, 0).is_none());
        assert!(windows.next_update_for(ids[2], &events, 0).is_none());
        let exposed = windows.begin_update(ids[1]).unwrap();
        windows.end_update(ids[1]);
        let expected = expected.unwrap();
        if !refuse
        {
            assert_eq!(exposed, expected, "healthy batch exposure differs from individual hiding");
        }
        else
        {
            assert!(exposed.left <= expected.left && exposed.top <= expected.top);
            assert!(exposed.right >= expected.right && exposed.bottom >= expected.bottom);
            let bounds = windows.window(ids[1]).unwrap().local_bounds();
            assert!(exposed.left >= bounds.left && exposed.top >= bounds.top);
            assert!(exposed.right <= bounds.right && exposed.bottom <= bounds.bottom);
        }
        assert!(events.next_matching(u16::MAX).is_none());
        let observe = Observe::begin(true, None);
        let changed = windows.hide_other_owners(owner, |_| true);
        let allocations = observe.finish();
        assert!(!changed);
        assert_eq!(allocations, 0);
        windows.set_owner_hidden(windows.window(ids[0]).unwrap().owner, false);
        assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(), ids);
    }
}

fn retained_hide_fixture() -> (WindowManager, EventManager, [mac_abi::WindowId; 4])
{
    let mut windows = WindowManager::new();
    let mut events = EventManager::new();
    let descriptors = [(Rect::new(40, 70, 300, 250), "Foreground back", 1),
                       (Rect::new(100, 90, 220, 270), "Retained stopped owner", 3),
                       (Rect::new(50, 110, 260, 230), "Running other", 2),
                       (Rect::new(350, 60, 500, 200), "Foreground front", 1)];
    let ids = core::array::from_fn(|index| {
        let (bounds, name, owner) = descriptors[index];
        windows.new_window(bounds, name, owner, (40, 40), &mut events, 0).unwrap()
    });
    for id in ids
    {
        windows.validate(id).unwrap();
    }
    while events.next_matching(u16::MAX).is_some()
    {}
    (windows, events, ids)
}

#[test]
fn hide_others_retains_noneligible_planes_as_survivors_in_exposure_and_occlusion()
{
    let (mut reference, reference_events, reference_ids) = retained_hide_fixture();
    let other = reference.window(reference_ids[2]).unwrap().owner;
    reference.set_owner_hidden(other, true);
    let back_damage = reference.begin_update(reference_ids[0]).unwrap();
    let retained_damage = reference.begin_update(reference_ids[1]).unwrap();
    assert!(reference.next_update_for(reference_ids[3], &reference_events, 0).is_none());
    for refuse in [false, true]
    {
        let (mut windows, mut events, ids) = retained_hide_fixture();
        let owner = windows.window(ids[3]).unwrap().owner;
        let other = windows.window(ids[2]).unwrap().owner;
        let retained = windows.window(ids[1]).unwrap().owner;
        let focus = windows.active_window();
        let observe = Observe::begin(refuse, None);
        let changed = windows.hide_other_owners(owner, |candidate| candidate == other);
        let allocations = observe.finish();
        assert!(changed);
        assert!(allocations > 0);
        assert_eq!(windows.active_window(), focus);
        assert_eq!(windows.back_to_front().map(|w| w.id).collect::<Vec<_>>(),
                   [ids[0], ids[1], ids[3]]);
        assert_eq!(windows.front_for_owner(retained), Some(ids[1]));
        assert_eq!(windows.front_for_owner(other), None);
        assert!(windows.next_update_for(ids[3], &events, 0).is_none());
        for (id, expected) in [(ids[0], back_damage), (ids[1], retained_damage)]
        {
            let actual = windows.begin_update(id).unwrap();
            windows.end_update(id);
            if !refuse
            {
                assert_eq!(actual, expected);
            }
            else
            {
                assert!(actual.left <= expected.left && actual.top <= expected.top);
                assert!(actual.right >= expected.right && actual.bottom >= expected.bottom);
                let bounds = windows.window(id).unwrap().local_bounds();
                assert!(actual.left >= bounds.left && actual.top >= bounds.top);
                assert!(actual.right <= bounds.right && actual.bottom <= bounds.bottom);
            }
        }
        assert!(events.next_matching(u16::MAX).is_none());
        let observe = Observe::begin(true, None);
        let changed = windows.hide_other_owners(owner, |candidate| candidate == other);
        let allocations = observe.finish();
        assert!(!changed);
        assert_eq!(allocations, 0);
        assert_eq!(windows.front_for_owner(retained), Some(ids[1]));
    }
}
