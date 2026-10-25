//! Actual Finder source over observed public SDK calls forwarded to real System.
#![cfg(target_arch = "riscv64")]
use core::{fmt::Write,
           sync::atomic::{AtomicU64, Ordering}};

use mac_toolbox::{events::{EventMask, Input, Modifiers},
                  *};

use crate::browser::{Browser, Location, Text};
static FULL_UPDATES: AtomicU64 = AtomicU64::new(0);
static FILES: AtomicU64 = AtomicU64::new(0);
static WIDTHS: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);
static COMMITS: AtomicU64 = AtomicU64::new(0);
static SCROLL_WINDOW: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BARS: spinning_top::Spinlock<[abi::controls::ScrollbarState; 2]> =
    spinning_top::Spinlock::new([abi::controls::ScrollbarState { bounds:      Rect::ZERO,
                                                                value:       0,
                                                                maximum:     0,
                                                                active:      0,
                                                                highlighted: 0,
                                                                reserved:    0 };
                                 2]);
/// # Safety
/// A successful synchronous public SDK call still owns this request buffer.
pub unsafe fn observed(number: u32, pointer: *mut (), size: usize)
{
    if number == abi::traps::CONTROL && size == core::mem::size_of::<abi::controls::ControlCall>()
    {
        let call = unsafe { &*pointer.cast::<abi::controls::ControlCall>() };
        if call.window == SCROLL_WINDOW.load(Ordering::Relaxed) && !call.scrollbar.bounds.is_empty()
        {
            let bounds = call.scrollbar.bounds;
            let horizontal = bounds.right - bounds.left > bounds.bottom - bounds.top;
            BARS.lock()[usize::from(horizontal)] = call.scrollbar;
        }
    }
}
/// # Safety
/// Caller passes the public SDK's live, synchronously borrowed request.
pub unsafe fn observe(number: u32, pointer: *mut (), size: usize)
{
    if number == abi::traps::WINDOW
       && size == core::mem::size_of::<abi::traps::WindowCall>()
       && unsafe { &*pointer.cast::<abi::traps::WindowCall>() }.operation == abi::BEGIN_UPDATE
    {
        FULL_UPDATES.fetch_add(1, Ordering::Relaxed);
    }
    if number == abi::traps::FILE
    {
        FILES.fetch_add(1, Ordering::Relaxed);
    }
    if number == abi::traps::FONT && size == core::mem::size_of::<abi::quickdraw::FontCall>()
    {
        let call = unsafe { &*pointer.cast::<abi::quickdraw::FontCall>() };
        if call.operation == abi::quickdraw::FONT_WIDTH
        {
            WIDTHS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if [abi::traps::QUICKDRAW,abi::traps::ICON,abi::traps::FONT,abi::traps::COPY_BITS].contains(&number){DRAWS.fetch_add(1,Ordering::Relaxed);}
    if number == abi::traps::QUICKDRAW
       && size == core::mem::size_of::<abi::quickdraw::GraphicsCall>()
    {
        let call = unsafe { &*pointer.cast::<abi::quickdraw::GraphicsCall>() };
        if call.operation == abi::quickdraw::QD_WINDOW_COMMIT_MASK
        {
            COMMITS.fetch_add(1, Ordering::Relaxed);
        }
    }
}
fn require(system: &System<'_>, condition: bool, reason: &str) -> Result<(), Error>
{
    if condition
    {
        return Ok(());
    }
    system.log("FINDER_SELECTION_FAIL ")?;
    system.log(reason)?;
    system.log("\n")?;
    Err(Error::BadArgument)
}
fn input(at: Point, when: u32, shift: bool) -> Input
{
    Input { at,
            when,
            modifiers: if shift { Modifiers::SHIFT } else { Modifiers::from_bits_retain(0) } }
}
fn pointer_click(system: &System<'_>, browser: &mut Browser, event: &Event) -> Result<(), Error>
{
    browser.click(system, event, system.events().locate(event)?).map(|_| ())
}
fn pointer_track(system: &System<'_>, browser: &mut Browser, event: &Event) -> Result<(), Error>
{
    browser.track(system, event, system.events().locate(event)?)
}

fn verify_positions(system: &System<'_>) -> Result<(), Error>
{
    let mut window =
        system.windows()
              .create_owned(windows::WindowSpec::new(Rect::new(100, 100, 300, 240),
                                                     "Input coordinates").procedure(4))?;
    let id = window.id();
    let event = Event::MouseDown { window: Some(id),
                                   input:  input(Point { h: 125,
                                                         v: 135 },
                                                 88,
                                                 true) };
    let record = event.as_record();
    system.events().post(event)?;
    let first = system.events().peek_positioned(EventMask::MOUSE_DOWN)?.ok_or(Error::NotFound)?;
    let again = system.events().peek_positioned(EventMask::MOUSE_DOWN)?.ok_or(Error::NotFound)?;
    require(system,
            first.event().into_record() == record && first.position() == again.position(),
            "coordinate peek changed input")?;
    let (received, position) = system.next_positioned(EventMask::MOUSE_DOWN)?.into_parts();
    require(system,
            received.as_record() == record
            && position.screen().point() == record.where_
            && position.in_window(id)?.point()?
               == (Point { h: 25,
                           v: 35 }),
            "incorrect event coordinate spaces")?;
    let other = system.windows()
                      .create_owned(windows::WindowSpec::new(Rect::new(150, 150, 350, 290),
                                                             "Other coordinates").procedure(4))?;
    let button = system.controls()
                       .create_standard(other.id(),
                                        controls::StandardKind::Button,
                                        controls::StandardState { bounds: Rect::new(10, 10, 80, 30),
                                                                  enabled: 1,
                                                                  ..Default::default() })?;
    require(system,
            button.control().track_at(position.in_window(id)?, |_| Ok(()))
            == Err(Error::BadArgument),
            "control accepted another window's position")?;
    drop(button);
    drop(other);
    let outside = Event::MouseDown { window: Some(id),
                                     input:  input(Point { h: i16::MIN,
                                                           v: 0 },
                                                   89,
                                                   false) };
    require(system,
            system.events().locate(&outside)?.in_window(id)?.point() == Err(Error::BadArgument),
            "local coordinate overflow wrapped")?;
    // Leave a click queued while the transport moves the window. Delivery must
    // preserve the original local position rather than using the new origin.
    system.events().post(Event::from(record))?;
    system.log("DESKTOP_READY: positioned event fixture\nEVENT_COORDINATES_MOVE_READY\n")?;
    loop
    {
        if matches!(system.wait_event(EventMask::KEY_DOWN, 30)?, Event::KeyDown { .. })
        {
            break;
        }
    }
    let moved = system.window(id)?.bounds;
    let (_, captured) = system.next_positioned(EventMask::MOUSE_DOWN)?.into_parts();
    require(system,
            moved.left == 140
            && moved.top == 140
            && captured.in_window(id)?.point()?
               == (Point { h: 25,
                           v: 35 }),
            "queued click used delivery-time geometry")?;
    require(system,
            system.events().locate(&Event::from(record))?.in_window(id)?.point()?
            == (Point { h: -15,
                        v: -5 }),
            "synthetic projection did not use current geometry")?;
    // Original native consumers still read the exact global EventRecord.
    system.events().post(Event::from(record))?;
    require(system,
            system.next_event(EventMask::MOUSE_DOWN)?.as_record() == record,
            "old native input changed")?;
    system.events().post(Event::from(record))?;
    window.close()?;
    let replacement =
        system.windows().create_owned(windows::WindowSpec::new(Rect::new(120, 120, 320, 260),
                                                                "Replacement").procedure(4))?;
    let (_, stale) = system.next_positioned(EventMask::MOUSE_DOWN)?.into_parts();
    require(system,
            replacement.id() != id
            && stale.window().is_none()
            && stale.in_window(replacement.id()) == Err(Error::NotFound),
            "stale event acquired a reused window")?;
    require(system,
            matches!(system.events().input_in(id), Err(Error::NotFound)),
            "disposed coordinate query succeeded")?;
    let foreign = Event::MouseDown { window: Some(WindowId(u64::MAX)),
                                     input:  input(Point { h: 125,
                                                           v: 135 },
                                                   90,
                                                   false) };
    require(system,
            system.events().locate(&foreign)?.window().is_none(),
            "foreign coordinate query leaked a window")?;
    drop(replacement);
    system.log("EVENT_COORDINATES_USERSPACE_PASS\n")
}

fn checkpoint(system: &System<'_>, stage: &str) -> Result<(), Error>
{
    system.log("FINDER_SELECTION_STAGE ")?;
    system.log(stage)?;
    system.log("\n")?;
    loop
    {
        if matches!(system.wait_event(EventMask::KEY_DOWN, 30)?, Event::KeyDown { .. })
        {
            return Ok(());
        }
    }
}
fn fill(system: &System<'_>, browser: &mut Browser) -> Result<(), Error>
{
    let mut background = crate::browser::dragging::Controller::default();
    for _ in 0..20000
    {
        if !browser.pending() && !background.positions_pending(browser)
        {
            return Ok(());
        }
        background.advance_positions(system, browser)?;
        browser.advance_cache(system)?;
        let event = system.next_event(EventMask::ALL)?;
        if matches!(event, Event::Activate { .. })
        {
            browser.activate(&event);
        }
        if let Event::Update { window,
                               .. } = event
           && browser.contains(window)
        {
            browser.draw(system, window)?;
        }
    }
    Err(Error::BadArgument)
}

fn verify_scrolling(system: &System<'_>) -> Result<(), Error>
{
    // Earlier checkpoints are already captured. Dispose their actual windows
    // before reusing the Finder's bounded app-private snapshot slots.
    for slot in 0..8
    {
        let id = crate::cache::with(slot, |folder| folder.window);
        if id.0 != 0 && system.window(id).is_ok()
        {
            system.close_window(id)?;
        }
    }
    let volume = system.files().volume_at(0)?.id;
    let folder = system.files().new_folder(volume, files::ROOT_ID, "Scroll Lab")?;
    let name = "A long filename with wrapped text.txt";
    let file = system.files()
                     .create_data(files::DataFileSpec { volume,
                                                        parent: folder.id,
                                                        name,
                                                        replacing: None,
                                                        file_type: (*b"TEXT").into(),
                                                        creator: (*b"TEST").into() },
                                  0)?
                     .finish()?;
    let caption = crate::icon_view::Caption::measure(name, |text| {
        system.quickdraw().fonts().text_width(quickdraw::fonts::FontSelection { family: 3,
                                                                                size:   9,
                                                                                face:   0 },
                                              text)
    })?;
    // The full wrapped label ends exactly one pixel past the initial viewport.
    let location = Point { h: 0,
                           v: 174 - 28 - 32 - caption.height() };
    system.files().set_icon_location(volume, file.id, Some(location))?;
    let mut browser = Browser::default();
    browser.initialize(system)?;
    fill(system, &mut browser)?;
    let id = browser.open(system,
                          Location { volume,
                                     folder: folder.id })?;
    fill(system, &mut browser)?;
    SCROLL_WINDOW.store(id.0, Ordering::Relaxed);
    browser.draw(system, id)?;
    require(system,
            BARS.lock()[0].maximum == 1,
            "one-pixel label overflow did not enable scrollbar")?;
    let full = FULL_UPDATES.load(Ordering::Relaxed);
    let files = FILES.load(Ordering::Relaxed);
    let widths = WIDTHS.load(Ordering::Relaxed);
    let commits = COMMITS.load(Ordering::Relaxed);
    browser.scroll(system, 1)?;
    let vertical = BARS.lock()[0];
    require(system,
            vertical.maximum == 0 && vertical.value == 0,
            "scrollbar retained empty travel after all content fit")?;
    require(system,
            FULL_UPDATES.load(Ordering::Relaxed) == full
            && FILES.load(Ordering::Relaxed) == files
            && WIDTHS.load(Ordering::Relaxed) == widths
            && COMMITS.load(Ordering::Relaxed) == commits + 1,
            "scrolling recomputed metadata/text or repainted outside its masked viewport")?;
    let draws = DRAWS.load(Ordering::Relaxed);
    browser.scroll(system, 52)?;
    browser.scroll(system, -52)?;
    require(system,
            DRAWS.load(Ordering::Relaxed) == draws
            && COMMITS.load(Ordering::Relaxed) == commits + 1,
            "no-change scrolling redrew content or controls")?;
    let horizontal = BARS.lock()[1];
    require(system,
            horizontal.maximum > 0,
            "left-clipped label did not enable horizontal scrolling")?;
    let frame = system.window(id)?.bounds;
    let at = Point { h: frame.left + horizontal.bounds.left + 7,
                     v: frame.top + horizontal.bounds.top + 7 };
    let event = Event::MouseDown { window: Some(id),
                                   input:  input(at, 42, false) };
    pointer_click(system, &mut browser, &event)?;
    let after = BARS.lock()[1];
    let mut probe = Text::<180>::default();
    let _ = writeln!(&mut probe,
                     "FINDER_SCROLL_CONTROL at={},{} before={},{},{} after={},{},{}",
                     at.h,
                     at.v,
                     horizontal.value,
                     horizontal.maximum,
                     horizontal.active,
                     after.value,
                     after.maximum,
                     after.active);
    system.log(probe.as_str())?;
    require(system,
            BARS.lock()[1].maximum == 0,
            "managed horizontal arrow did not reveal the full caption and reset its range")?;
    require(system,
            FULL_UPDATES.load(Ordering::Relaxed) == full
            && FILES.load(Ordering::Relaxed) == files
            && WIDTHS.load(Ordering::Relaxed) == widths
            && COMMITS.load(Ordering::Relaxed) == commits + 2,
            "horizontal control tracking repainted/re-enumerated beyond the icon viewport")?;
    let other = system.files()
                      .create_data(files::DataFileSpec { volume,
                                                         parent: folder.id,
                                                         name: "Distant item",
                                                         replacing: None,
                                                         file_type: (*b"TEXT").into(),
                                                         creator: (*b"TEST").into() },
                                   0)?
                      .finish()?;
    system.files().set_icon_location(volume,
                                      other.id,
                                      Some(Point { h: 0,
                                                   v: 500 }))?;
    browser.folder_changed(system, id)?;
    fill(system, &mut browser)?;
    require(system,
            BARS.lock()[0].maximum > 300,
            "new distant item did not expand the scroll range")?;
    system.files().move_to_trash(volume, other.id)?;
    browser.folder_changed(system, id)?;
    fill(system, &mut browser)?;
    require(system,
            BARS.lock()[0].maximum == 0,
            "removing distant item did not contract the scroll range")?;
    SCROLL_WINDOW.store(0, Ordering::Relaxed);
    system.log("FINDER_SCROLL_USERSPACE_PASS label_overflow=1 bounded=true reset=true\n")
}
fn action(system: &System<'_>, browser: &mut Browser, label: &str, count: usize, noop: bool,
          run: impl FnOnce(&mut Browser) -> Result<(), Error>)
          -> Result<(), Error>
{
    let before = [FILES.load(Ordering::Relaxed),
                  WIDTHS.load(Ordering::Relaxed),
                  DRAWS.load(Ordering::Relaxed),
                  COMMITS.load(Ordering::Relaxed)];
    let start = system.events().monotonic_nanoseconds()?;
    run(browser)?;
    let elapsed = system.events().monotonic_nanoseconds()?.saturating_sub(start);
    require(system, browser.selection_count() == count, "wrong selection membership")?;
    require(system, FILES.load(Ordering::Relaxed) == before[0], "pointer path called Files")?;
    require(system, WIDTHS.load(Ordering::Relaxed) == before[1], "pointer path remeasured text")?;
    if noop
    {
        require(system, DRAWS.load(Ordering::Relaxed) == before[2], "unchanged state drew pixels")?;
    }
    require(system, elapsed < 500_000_000, "selection exceeded watch deadline")?;
    let mut log = Text::<200>::default();
    let _ = writeln!(&mut log,
                     "FINDER_SELECTION_ACTION name={} selected={} files=0 widths=0 draws={} \
                      commits={} ns={}",
                     label,
                     count,
                     DRAWS.load(Ordering::Relaxed) - before[2],
                     COMMITS.load(Ordering::Relaxed) - before[3],
                     elapsed);
    system.log(log.as_str())
}
pub fn run(system: &System<'_>) -> Result<(), Error>
{
    let startup = system.files().volume_at(0)?.id;
    if system.files().lookup(startup, files::ROOT_ID, "Scroll only").is_ok()
    {
        system.log("DESKTOP_READY: Finder scroll fixture\n")?;
        verify_scrolling(system)?;
        loop
        {
            system.wait_event(EventMask::from_bits_retain(0), 30)?;
        }
    }
    verify_positions(system)?;
    let code = system.resources().application().get(ResourceKey { kind: TypeCode::new(*b"RV64"),
                                                                   id:   0 })?;
    let stored = code.size();
    code.release()?;
    require(system, stored < 512 * 1024, "cache storage inflated the code resource")?;
    let volume = system.files().volume_at(0)?.id;
    let folder = system.files().new_folder(volume, files::ROOT_ID, "Selection Lab")?;
    for index in 0..8
    {
        let mut name = Text::<32>::default();
        let _ = write!(&mut name, "Item {index:02}");
        system.files()
              .create_data(files::DataFileSpec { volume,
                                                 parent: folder.id,
                                                 name: name.as_str(),
                                                 replacing: None,
                                                 file_type: TypeCode::new(*b"TEXT"),
                                                 creator: TypeCode::new(*b"ttxt") },
                           0)?
              .finish()?;
    }
    let mut browser = Browser::default();
    browser.initialize(system)?;
    fill(system, &mut browser)?;
    let lab = browser.open(system,
                           Location { volume,
                                      folder: folder.id })?;
    fill(system, &mut browser)?;
    let other = system.files().new_folder(volume, files::ROOT_ID, "Other Window")?;
    browser.open(system,
                 Location { volume,
                            folder: other.id })?;
    fill(system, &mut browser)?;
    system.select_window(lab)?;
    browser.activate(&Event::Activate { window: lab,
                                        active: true,
                                        input:  input(Point::default(), 0, false).with_active() });
    browser.draw(system, lab)?;
    system.quickdraw().cursors().hide()?;
    system.log("DESKTOP_READY: Finder cached selection fixture\n")?;
    checkpoint(system, "baseline")?;
    let bounds = system.window(lab)?.bounds;
    let width =
        system.quickdraw().fonts().text_width(quickdraw::fonts::FontSelection { family: 3,
                                                                                 size:   9,
                                                                                 face:   0 },
                                               "Item 00")?;
    let mut geometry = Text::<160>::default();
    let first = crate::icon_view::label_bounds(Rect::new(16, 28, 48, 60), width);
    let second = crate::icon_view::label_bounds(Rect::new(80, 28, 112, 60), width);
    let _ = writeln!(&mut geometry,
                     "FINDER_SELECTION_BOUNDS {} {} {} {} {} {} {} {}",
                     bounds.left + 16.min(first.left),
                     bounds.top + 28,
                     bounds.left + 48.max(first.right),
                     bounds.top + first.bottom,
                     bounds.left + 80.min(second.left),
                     bounds.top + 28,
                     bounds.left + 112.max(second.right),
                     bounds.top + second.bottom);
    system.log(geometry.as_str())?;
    let point = |h, v| {
        Point { h: bounds.left + h,
                v: bounds.top + v }
    };
    let click = |browser: &mut Browser, h, v, when, shift| {
        pointer_click(system,
                      browser,
                      &Event::MouseDown { window: Some(lab),
                                          input:  input(point(h, v), when, shift) })
    };
    action(system, &mut browser, "single", 1, false, |browser| click(browser, 32, 44, 100, false))?;
    checkpoint(system, "single")?;
    action(system, &mut browser, "shift-add", 2, false, |browser| {
        click(browser, 96, 44, 200, true)
    })?;
    checkpoint(system, "shift-add")?;
    action(system, &mut browser, "selected-noop", 2, true, |browser| {
        click(browser, 96, 44, 300, false)
    })?;
    checkpoint(system, "selected-noop")?;
    action(system, &mut browser, "shift-toggle", 1, false, |browser| {
        click(browser, 32, 44, 400, true)
    })?;
    checkpoint(system, "shift-toggle")?;
    action(system, &mut browser, "clear", 0, false, |browser| click(browser, 4, 24, 500, false))?;
    let move_band = |browser: &mut Browser, h, v, when, up| {
        let event = if up
        {
            Event::MouseUp { window: Some(lab),
                             input:  input(point(h, v), when, false) }
        }
        else
        {
            Event::Idle { input: input(point(h, v), when, false),
                          tag:   0 }
        };
        pointer_track(system, browser, &event)
    };
    action(system, &mut browser, "marquee-three", 3, false, |browser| {
        move_band(browser, 182, 76, 501, false)
    })?;
    checkpoint(system, "marquee-three")?;
    action(system, &mut browser, "marquee-stationary", 3, true, |browser| {
        move_band(browser, 182, 76, 502, false)
    })?;
    action(system, &mut browser, "marquee-shrink", 1, false, |browser| {
        move_band(browser, 55, 76, 503, false)
    })?;
    checkpoint(system, "marquee-shrink")?;
    action(system, &mut browser, "marquee-release", 1, false, |browser| {
        move_band(browser, 55, 76, 504, true)
    })?;
    checkpoint(system, "marquee-release")?;
    action(system, &mut browser, "select-all", 8, false, |browser| browser.select_all(system))?;
    checkpoint(system, "select-all")?;
    action(system, &mut browser, "select-all-noop", 8, true, |browser| browser.select_all(system))?;
    action(system, &mut browser, "shift-band-start", 8, true, |browser| {
        click(browser, 4, 24, 600, true)
    })?;
    let before = COMMITS.load(Ordering::Relaxed);
    for step in 0..2000
    {
        move_band(&mut browser, 55 + (step % 128) as i16, 76, 601 + step, false)?;
        system.next_event(EventMask::from_bits_retain(0))?;
    }
    require(system,
            COMMITS.load(Ordering::Relaxed) == before,
            "outline motion rebuilt retained content")?;
    move_band(&mut browser, 182, 76, 3001, true)?;
    require(system, browser.selection_count() == 8, "shift marquee lost base selection")?;
    checkpoint(system, "outline-stress")?;
    // A folder revision invalidates this snapshot and preserves selected IDs.
    system.files().new_folder(volume, folder.id, "New Arrival")?;
    browser.folder_changed(system, lab)?;
    browser.advance_cache(system)?;
    // Invalidate an in-flight cursor. Its unpublished partial result must be
    // discarded, and stable selections must survive the restarted snapshot.
    system.files().new_folder(volume, folder.id, "Racing Arrival")?;
    fill(system, &mut browser)?;
    require(system, browser.selection_count() == 8, "refresh lost stable selected IDs")?;
    action(system, &mut browser, "select-new-snapshot", 10, false, |browser| {
        browser.select_all(system)
    })?;
    checkpoint(system, "refreshed")?;
    // Force the cache-invalidation interval deterministically. Pointer replay
    // must leave the browser and traverse the application's shared dispatcher.
    let selected_before_refresh = browser.selected_locations();
    browser.folder_changed(system, lab)?;
    require(system,
            browser.selected_locations() == selected_before_refresh
            && browser.info_selection(system) == selected_before_refresh[0],
            "background folder refresh discarded the visibly selected Get Info targets")?;
    let waiting_down = Event::MouseDown { window: Some(lab),
                                          input:  input(point(32, 44), 5000, false) };
    let admitted = browser.click(system, &waiting_down, system.events().locate(&waiting_down)?)?;
    require(system,
            admitted == crate::browser::ClickOutcome::Deferred,
            "queued click incorrectly published completion")?;
    let up = Event::MouseUp { window: Some(lab),
                              input:
                                  Input { at:        point(32, 44),
                                          when:      5001,
                                          modifiers:
                                              Modifiers::from_bits_retain(abi::events::OPTION_KEY
                                                                          | abi::events::BTN_STATE) } };
    pointer_track(system, &mut browser, &up)?;
    fill(system, &mut browser)?;
    let (down, down_position) = browser.next_deferred_pointer().ok_or(Error::NotFound)?;
    require(system,
            matches!(down, Event::MouseDown { .. }) && down.input().when == 5000,
            "deferred press did not reach main dispatch")?;
    require(system,
            browser.click(system, &down, down_position)?
            == crate::browser::ClickOutcome::Completed,
            "ready deferred click did not complete on replay")?;
    let (received_up, up_position) = browser.next_deferred_pointer().ok_or(Error::NotFound)?;
    require(system,
            received_up.as_record() == up.as_record(),
            "deferred release lost its sampled modifiers")?;
    browser.track(system, &received_up, up_position)?;
    require(system,
            browser.next_deferred_pointer().is_none(),
            "deferred pointer replay duplicated an event")?;
    browser.select_all(system)?;
    require(system, browser.selection_count() == 10, "deferred replay corrupted the snapshot")?;
    system.log("FINDER_DEFERRED_POINTER_USERSPACE_PASS ordered=passed modifiers=passed\n")?;
    let mut info = crate::get_info::Controller::default();
    info.open_selection(system, browser.selected_locations())?;
    for _ in 0..2000
    {
        if !info.pending()
        {
            break;
        }
        info.event(system,
                   &Event::Idle { input: input(Point::default(), 4000, false),
                                  tag:   0 },
                   PointerPosition::default())?;
        system.next_event(EventMask::from_bits_retain(0))?;
    }
    require(system, !info.pending(), "multiple info did not finish bounded totals")?;
    require(system,
            system.window(WindowId(0))?.title() == "Multiple Items",
            "multiple info has wrong title")?;
    checkpoint(system, "multiple-info")?;
    let wrapped = system.files().new_folder(volume, files::ROOT_ID, "Wrapped Captions")?;
    let names = ["A long filename with several words and its final extension.txt",
                 "AReallyLongUnbrokenFileNameThatMustWrapWithoutDroppingItsExtension.rs",
                 "Éléphant 🦀 avec un très long nom de fichier.txt"];
    for name in names
    {
        system.files()
              .create_data(files::DataFileSpec { volume,
                                                 parent: wrapped.id,
                                                 name,
                                                 replacing: None,
                                                 file_type: TypeCode::new(*b"TEXT"),
                                                 creator: TypeCode::new(*b"ttxt") },
                           0)?
              .finish()?;
    }
    let wrapped_window = browser.open(system,
                                      Location { volume,
                                                 folder: wrapped.id })?;
    fill(system, &mut browser)?;
    browser.activate(&Event::Activate { window: wrapped_window,
                                 active: true,
                                 input:  input(Point::default(), 5000, false).with_active() });
    browser.draw(system, wrapped_window)?;
    let (index, item) =
        (0..8).find_map(|slot| {
                  crate::cache::with(slot, |folder| {
                      (folder.window == wrapped_window).then(|| (0, folder.items[0]))
                  })
              })
              .ok_or(Error::BadArgument)?;
    require(system, index == 0 && item.caption.height() > 12, "long caption did not wrap")?;
    require(system,
            item.caption.width() <= crate::icon_view::CAPTION_WIDTH,
            "caption grew beyond width cap")?;
    let mut joined = Text::<256>::default();
    for (line, _) in item.caption.lines(item.entry.name())
    {
        let _ = write!(&mut joined, "{line}");
    }
    require(system, joined.as_str() == item.entry.name(), "caption lost filename bytes")?;
    checkpoint(system, "wrapped")?;
    let frame = system.window(wrapped_window)?.bounds;
    let label = item.caption.bounds(item.icon_bounds);
    let last_line = Point { h: frame.left + (label.left + label.right) / 2,
                            v: frame.top + label.bottom - 6 };
    require(system,
            last_line.v > frame.top + item.icon_bounds.bottom + 12,
            "wrapped hit test did not reach a later line")?;
    let mut geometry = Text::<160>::default();
    let _ = writeln!(&mut geometry,
                     "FINDER_WRAPPED_BOUNDS {} {} {} {}",
                     frame.left + item.bounds.left,
                     frame.top + item.bounds.top,
                     frame.left + item.bounds.right,
                     frame.top + item.bounds.bottom);
    system.log(geometry.as_str())?;
    for (name, when, noop) in [("wrapped-last-line", 5100, false), ("wrapped-noop", 5200, true)]
    {
        action(system, &mut browser, name, 1, noop, |browser| {
            pointer_click(system,
                          browser,
                          &Event::MouseDown { window: Some(wrapped_window),
                                              input:  input(last_line, when, false) })
        })?;
        checkpoint(system, if noop { "wrapped-noop" } else { "wrapped-selected" })?;
    }
    // A physical click during refresh must be replayed after publication,
    // rather than disappearing between the watch notification and cache fill.
    browser.folder_changed(system, wrapped_window)?;
    action(system, &mut browser, "wrapped-refresh-press", 0, true, |browser| {
        pointer_click(system,
                      browser,
                      &Event::MouseDown { window: Some(wrapped_window),
                                          input:  input(last_line, 5400, false) })
    })?;
    action(system, &mut browser, "wrapped-refresh-release", 0, true, |browser| {
        pointer_track(system,
                      browser,
                      &Event::MouseUp { window: Some(wrapped_window),
                                        input:  input(last_line, 5401, false) })
    })?;
    fill(system, &mut browser)?;
    require(system, browser.selection_count() == 1, "refresh lost a physical click")?;
    checkpoint(system, "wrapped-refreshed")?;
    let desktop_click = |browser: &mut Browser, at, when, shift| {
        pointer_click(system,
                      browser,
                      &Event::MouseDown { window: None,
                                          input:  input(at, when, shift) })
    };
    let root_point = Point { h: 600,
                             v: 44 };
    let trash_point = Point { h: 592,
                              v: 436 };
    let empty = Point { h: 540,
                        v: 400 };
    action(system, &mut browser, "desktop-single", 1, false, |browser| {
        desktop_click(browser, root_point, 6000, false)
    })?;
    checkpoint(system, "desktop-single")?;
    let trash = crate::browser::desktop::item(crate::browser::desktop::count() - 1).unwrap();
    let mut geometry = Text::<160>::default();
    let _ = writeln!(&mut geometry,
                     "FINDER_DESKTOP_TRASH_BOUNDS {} {} {} {}",
                     trash.bounds.left, trash.bounds.top, trash.bounds.right, trash.bounds.bottom);
    system.log(geometry.as_str())?;
    action(system, &mut browser, "desktop-shift", 2, false, |browser| {
        desktop_click(browser, trash_point, 6100, true)
    })?;
    checkpoint(system, "desktop-shift")?;
    action(system, &mut browser, "desktop-clear", 0, false, |browser| {
        desktop_click(browser, empty, 6200, false)
    })?;
    checkpoint(system, "desktop-clear")?;
    let band_end = Point { h: 630,
                           v: 474 };
    action(system, &mut browser, "desktop-band-trash", 1, false, |browser| {
        pointer_track(system,
                      browser,
                      &Event::Idle { input: input(band_end, 6201, false),
                                     tag:   0 })
    })?;
    checkpoint(system, "desktop-band-trash")?;
    action(system, &mut browser, "desktop-band-stationary", 1, true, |browser| {
        pointer_track(system,
                      browser,
                      &Event::Idle { input: input(band_end, 6202, false),
                                     tag:   0 })
    })?;
    action(system, &mut browser, "desktop-band-cancel", 0, false, |browser| {
        pointer_track(system,
                      browser,
                      &Event::from(abi::events::Event { payload:   0,
                                                        kind:      abi::events::EventKind::KeyDown,
                                                        message:   0x351b,
                                                        when:      6203,
                                                        where_:    band_end,
                                                        modifiers: 0,
                                                        window:    0 }))
    })?;
    checkpoint(system, "desktop-band-cancel")?;
    action(system, &mut browser, "desktop-all", 2, false, |browser| browser.select_all(system))?;
    checkpoint(system, "desktop-all")?;
    action(system, &mut browser, "desktop-all-noop", 2, true, |browser| {
        browser.select_all(system)
    })?;
    action(system, &mut browser, "desktop-shift-band", 2, true, |browser| {
        desktop_click(browser, empty, 6300, true)
    })?;
    let before = COMMITS.load(Ordering::Relaxed);
    for step in 0..2000
    {
        pointer_track(system,
                      &mut browser,
                      &Event::Idle { input: input(Point { h: 570 + (step % 60) as i16,
                                                          v: 474 },
                                                  6301 + step,
                                                  false),
                                     tag:   0 })?;
        system.next_event(EventMask::from_bits_retain(0))?;
    }
    require(system,
            COMMITS.load(Ordering::Relaxed) == before,
            "desktop outline rebuilt retained drawing")?;
    pointer_track(system,
                  &mut browser,
                  &Event::MouseUp { window: None,
                                    input:  input(band_end, 8401, false) })?;
    require(system, browser.selection_count() == 2, "desktop marquee lost base selection")?;
    checkpoint(system, "desktop-outline")?;
    action(system, &mut browser, "desktop-selected-press", 2, true, |browser| {
        desktop_click(browser, root_point, 8500, false)
    })?;
    action(system, &mut browser, "desktop-selected-release", 1, false, |browser| {
        pointer_track(system,
                      browser,
                      &Event::MouseUp { window: None,
                                        input:  input(root_point, 8501, false) })
    })?;
    checkpoint(system, "desktop-collapse")?;
    // Delivery of the previous folder's deactivate edge may occur between the
    // two desktop presses. It must preserve the desktop target's click pair.
    browser.activate(&Event::Activate { window: wrapped_window,
                                        active: false,
                                        input:  input(root_point, 8502, false) });
    desktop_click(&mut browser, root_point, 8510, false)?;
    let root_window = crate::cache::with(0, |folder| folder.window);
    require(system,
            system.window(WindowId(0))?.id == root_window,
            "deactivation erased desktop volume double-click")?;
    checkpoint(system, "desktop-double-open")?;
    let placed_folder = system.files().new_folder(volume, files::ROOT_ID, "Placement Lab")?;
    let placed_file =
        system.files()
              .create_data(files::DataFileSpec { volume,
                                                 parent: placed_folder.id,
                                                 name: "Position follows this long file \
                                                        name.txt",
                                                 replacing: None,
                                                 file_type: TypeCode::new(*b"TEXT"),
                                                 creator: TypeCode::new(*b"ttxt") },
                           0)?
              .finish()?;
    let saved = Point { h: 80,
                        v: 40 };
    let placed = system.files().set_icon_location(volume, placed_file.id, Some(saved))?;
    require(system,
            placed.id == placed_file.id
            && placed.modified == placed_file.modified
            && placed.created == placed_file.created,
            "placement changed identity or dates")?;
    let location = Location { volume,
                              folder: placed_folder.id };
    let first_window = browser.open(system, location)?;
    fill(system, &mut browser)?;
    let geometry = |window| {
        (0..8).find_map(|slot| crate::cache::with(slot, |folder| {
        (folder.window == window && folder.ready && folder.count == 1).then(|| folder.items[0].icon_bounds)
    }))
    };
    require(system,
            geometry(first_window) == Some(Rect::new(96, 68, 128, 100)),
            "saved placement was not rendered")?;
    checkpoint(system, "placement-saved")?;
    system.close_window(first_window)?;
    browser.prune(system)?;
    let reopened = browser.open(system, location)?;
    fill(system, &mut browser)?;
    require(system,
            reopened != first_window && geometry(reopened) == Some(Rect::new(96, 68, 128, 100)),
            "placement did not survive folder close/reopen")?;
    system.files().rename(volume, placed_file.id, "Renamed, same saved placement.txt")?;
    browser.folder_changed(system, reopened)?;
    fill(system, &mut browser)?;
    require(system,
            geometry(reopened) == Some(Rect::new(96, 68, 128, 100))
            && system.files().catalog(volume, placed_file.id)?.saved_icon_location()
               == Some(saved),
            "rename detached placement from file identity")?;
    checkpoint(system, "placement-reopened-renamed")?;
    let destination = system.files().new_folder(volume, files::ROOT_ID, "Placement Destination")?;
    let before = system.files().catalog(volume, placed_file.id)?;
    let moved = system.files().move_into(volume, placed_file.id, destination.id)?;
    require(system,
            moved.id == before.id
            && moved.finder_info == before.finder_info
            && moved.created == before.created
            && moved.modified == before.modified
            && moved.parent == destination.id,
            "folder move changed identity, placement or dates")?;
    browser.folder_changed(system, reopened)?;
    fill(system, &mut browser)?;
    require(system, geometry(reopened).is_none(), "moved file left a stale source icon")?;
    system.close_window(reopened)?;
    browser.prune(system)?;
    let destination_window = browser.open(system,
                                          Location { volume,
                                                     folder: destination.id })?;
    fill(system, &mut browser)?;
    require(system,
            geometry(destination_window) == Some(Rect::new(96, 68, 128, 100)),
            "folder move detached saved placement from its file")?;
    checkpoint(system, "placement-moved")?;
    // New files from another application arrive without Finder positions.
    // Existing manual placements are reserved before finding automatic holes.
    let mut fresh = [files::CatalogInfo::EMPTY; 2];
    for (slot, name) in fresh.iter_mut().zip(["New external file.txt", "New application file.txt"])
    {
        *slot = system.files()
                      .create_data(files::DataFileSpec { volume,
                                                         parent: destination.id,
                                                         name,
                                                         replacing: None,
                                                         file_type: TypeCode::new(*b"TEXT"),
                                                         creator: TypeCode::new(*b"ttxt") },
                                   0)?
                      .finish()?;
        require(system, slot.saved_icon_location().is_none(), "new file unexpectedly placed")?;
    }
    browser.folder_changed(system, destination_window)?;
    fill(system, &mut browser)?;
    for entry in &mut fresh
    {
        let stored = system.files().catalog(volume, entry.id)?;
        require(system,
                stored.saved_icon_location().is_some(),
                "first folder view did not save a new file position")?;
        require(system,
                stored.created == entry.created && stored.modified == entry.modified,
                "automatic placement changed object dates")?;
        *entry = stored;
    }
    let slot = (0..8).find(|&slot| crate::cache::with(slot, |f| f.window == destination_window))
                     .ok_or(Error::BadArgument)?;
    let removed_bounds = crate::cache::with(slot, |f| {
                             for a in 0..f.count
                             {
                                 for b in a + 1..f.count
                                 {
                                     if crate::cache::overlaps(f.items[a].bounds, f.items[b].bounds)
                                     {
                                         return None;
                                     }
                                 }
                             }
                             f.items[..f.count].iter()
                                               .find(|item| item.entry.id == placed_file.id)
                                               .map(|item| item.bounds)
                         }).ok_or(Error::BadArgument)?;
    checkpoint(system, "placement-auto-added")?;
    let full = FULL_UPDATES.load(Ordering::Relaxed);
    let widths = WIDTHS.load(Ordering::Relaxed);
    system.files().move_into(volume, placed_file.id, other.id)?;
    browser.folder_changed(system, destination_window)?;
    fill(system, &mut browser)?;
    require(system,
            FULL_UPDATES.load(Ordering::Relaxed) == full,
            "source removal repainted the whole window")?;
    require(system,
            WIDTHS.load(Ordering::Relaxed) - widths <= 2,
            "source refresh remeasured unchanged names")?;
    for entry in fresh
    {
        let stored = system.files().catalog(volume, entry.id)?;
        require(system,
                stored.saved_icon_location() == entry.saved_icon_location(),
                "source removal rearranged another file")?;
        require(system,
                stored.created == entry.created && stored.modified == entry.modified,
                "source removal changed another file's dates")?;
    }
    let frame = system.window(destination_window)?.bounds;
    let mut log = Text::<180>::default();
    let _ = writeln!(&mut log,
                     "FINDER_SOURCE_GAP_BOUNDS {} {} {} {} {} {} {} {}",
                     frame.left,
                     frame.top,
                     frame.right,
                     frame.bottom,
                     frame.left + removed_bounds.left,
                     frame.top + removed_bounds.top,
                     frame.left + removed_bounds.right,
                     frame.top + removed_bounds.bottom);
    system.log(log.as_str())?;
    checkpoint(system, "placement-source-gap")?;
    system.log("FINDER_SELECTION_USERSPACE_PASS stages=30 outline_steps=4000\n")?;
    loop
    {
        system.wait_event(EventMask::from_bits_retain(0), 30)?;
    }
}
trait ActiveInput
{
    fn with_active(self) -> Self;
}
impl ActiveInput for Input
{
    fn with_active(mut self) -> Self
    {
        self.modifiers =
            Modifiers::from_bits_retain(self.modifiers.bits() | Modifiers::ACTIVE.bits());
        self
    }
}
