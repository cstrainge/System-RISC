//! Native process acceptance through the real public SDK. These are native OS
//! threading extensions, distinct from the original cooperative scheduling model.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{ACCESS_DENIED, Event, EventKind, Rect, System, WindowId, events::EVERY_EVENT,
                  memory::Handle, processes::ProcessInfo, windows::WindowSpec};

#[derive(Debug)]
pub enum Failure
{
    Contract(&'static str),
    Service(i32)
}
impl From<mac_toolbox::Error> for Failure
{
    fn from(value: mac_toolbox::Error) -> Self
    {
        Self::Service(value.0)
    }
}
impl From<i32> for Failure
{
    fn from(value: i32) -> Self
    {
        Self::Service(value)
    }
}
#[path = "conformance/application_menu.rs"]
mod application_menu;
fn menu(system: &System<'_>) -> Result<()>
{
    application_menu::snapshot(system).map(|_| ())
}

type Result<T> = core::result::Result<T, Failure>;
const OBSERVER_SYNTHETIC: u32 = 0xa11c0001;
const WORKER_SYNTHETIC: u32 = 0xa11c0002;

fn synthetic_private(system: &System<'_>, foreign_window: WindowId, tag: u32) -> Result<()>
{
    let mask = (1 << EventKind::Activate as u16) | (1 << EventKind::MouseDown as u16);
    let events = system.events();
    // No physical input is injected during this handshake. Discard incidental
    // kernel activation records before isolating the exact caller-posted payload.
    events.flush(mask, 0)?;
    for kind in [EventKind::Activate, EventKind::MouseDown]
    {
        let posted = Event { kind,
                             message: foreign_window.0,
                             when: tag,
                             where_: mac_toolbox::Point { h: 7,
                                                          v: 9 },
                             modifiers: 0x5a00 };
        events.post(posted)?;
        require(events.peek(mask)? == Some(posted),
                "caller-posted foreign-window event escaped private queue")?;
        require(events.peek(mask)? == Some(posted),
                "private synthetic event peek consumed or changed payload")?;
        events.flush(mask, 0)?;
        require(events.peek(mask)?.is_none(), "private synthetic event flush did not remove it")?;
    }
    system.log(if tag == OBSERVER_SYNTHETIC
               {
                   "PROCESS_SYNTHETIC_EVENTS_PASS actor=observer cases=2\n"
               }
               else
               {
                   "PROCESS_SYNTHETIC_EVENTS_PASS actor=worker cases=2\n"
               })?;
    Ok(())
}
fn require(value: bool, message: &'static str) -> Result<()>
{
    if value { Ok(()) } else { Err(Failure::Contract(message)) }
}
pub fn resource(system: &System<'_>, expected: &str) -> Result<()>
{
    let mut output = [0; 64];
    require(system.resources().application().string(2000, &mut output)? == expected,
            "application resource partition")?;
    require(system.resources().search_chain().string(2000, &mut output)? == expected,
            "application search-chain partition")?;
    Ok(())
}
fn memory(handle: &Handle<'_>, expected: &[u8]) -> Result<()>
{
    let mut output = [0; 64];
    require(handle.len()? == expected.len(), "private handle length")?;
    let n = handle.read(0, &mut output)?;
    require(&output[..n] == expected, "private handle contents")
}
fn find(system: &System<'_>, id: u64) -> Result<Option<ProcessInfo>>
{
    let processes = system.processes();
    for i in 0..processes.count()?
    {
        let info = processes.at(i)?;
        if info.id == id
        {
            return Ok(Some(info));
        }
    }
    Ok(None)
}
fn state(system: &System<'_>, id: u64) -> Result<ProcessInfo>
{
    find(system, id)?.ok_or(Failure::Contract("missing live process"))
}
/// Multiwindow application activation is a single focus transition, not a
/// sequence of selections through every window in the application's plane.
fn atomic_activation(system: &System<'_>, own: u64, other: u64, front: WindowId) -> Result<()>
{
    let events = system.events();
    let activate = 1 << EventKind::Activate as u16;
    system.processes().activate(own)?;
    events.flush(activate, 0)?;
    system.processes().activate(other)?;
    system.processes().activate(own)?;
    for active in [false, true]
    {
        let event =
            events.next(activate)?.ok_or(Failure::Contract("missing atomic activation edge"))?;
        require(event.message == front.0
                && (event.modifiers & mac_toolbox::ACTIVE_FLAG != 0) == active,
                "application activation selected an intermediate window")?;
    }
    require(events.next(activate)?.is_none(), "extra application activation edge")?;
    // Acknowledge pending damage, then no-op activation/visibility requests
    // must not manufacture additional Update events.
    for _ in 0..32
    {
        let Some(event) = events.next(1 << EventKind::Update as u16)?
        else
        {
            break;
        };
        system.begin_update(event.message)?;
        system.end_update(event.message)?;
    }
    for _ in 0..3
    {
        system.processes().activate(own)?;
        system.processes().show_all()?;
    }
    require(events.next(activate)?.is_none(), "repeated activation generated events")?;
    require(events.next(1 << EventKind::Update as u16)?.is_none(),
            "unchanged visibility generated damage")?;
    require(system.window(0)?.id == front.0, "application window order changed")?;
    system.log("PROCESS_ATOMIC_ACTIVATION_PASS cases=5\n")?;
    Ok(())
}
fn pulse(system: &System<'_>, mask: u16) -> Result<()>
{
    let event = system.next_event(mask)?;
    require(event.when != WORKER_SYNTHETIC, "worker synthetic event reached foreign observer")?;
    require(event.kind != EventKind::KeyDown || event.character() != b'x',
            "foreground child's physical key leaked to observer")?;
    if event.kind == EventKind::Update && event.message != 0
    {
        system.windows().begin_update(WindowId(event.message))?;
        system.windows().end_update(WindowId(event.message))?;
    }
    Ok(())
}
fn wait(system: &System<'_>, condition: impl FnMut() -> Result<bool>) -> Result<()>
{
    wait_mask(system, condition, EVERY_EVENT & !(1 << EventKind::Application1 as u16))
}
fn wait_mask(system: &System<'_>, mut condition: impl FnMut() -> Result<bool>, mask: u16)
             -> Result<()>
{
    let start = system.events().uptime_ticks()?;
    loop
    {
        if condition()?
        {
            return Ok(());
        }
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 900,
                "bounded process wait expired")?;
        pulse(system, mask)?;
    }
}
fn parse_window(text: &str, prefix: &str) -> Option<WindowId>
{
    let suffix = text.strip_prefix(prefix)?;
    let mut value = 0u32;
    for byte in suffix.bytes()
    {
        value = value.checked_mul(10)?.checked_add(u32::from(byte.checked_sub(b'0')?))?;
        if !byte.is_ascii_digit()
        {
            return None;
        }
    }
    if suffix.is_empty() { None } else { Some(WindowId(value)) }
}
pub fn observer(system: &System<'_>) -> Result<()>
{
    system.log("DESKTOP_READY: public Process Manager acceptance\n")?;
    let processes = system.processes();
    let me = processes.current()?;
    require(me.id != 0 && me.thread != 0 && me.foreground != 0, "trusted observer identity")?;
    require(processes.count()? == 1, "initial process count")?;
    resource(system, "process-observer")?;
    menu(system)?;
    let handle = system.memory().handle_from_bytes(b"observer-owned memory")?;
    let own_window =
        system.windows()
              .create(WindowSpec::new(Rect::new(30, 60, 320, 170), "Process Observer"))?;
    let own_front =
        system.windows().create(WindowSpec::new(Rect::new(45, 80, 335, 190), "Observer Front"))?;
    let volume = system.files().volume_at(0)?.id;
    for (disk, cnid) in [(volume, 2), (volume, u32::MAX), (u32::MAX, 2)]
    {
        require(processes.launch(disk, cnid).is_err(), "invalid launch was accepted")?;
        require(processes.count()? == 1, "invalid launch leaked a process")?;
    }
    system.log("PROCESS_INVALID_LAUNCH_PASS\n")?;
    let spinner = system.files().lookup(volume, 2, "Applications/ProcessSpinner")?;
    let busy = processes.launch(volume, spinner.id)?;
    require(busy.id != me.id && busy.thread != me.thread,
            "spinner is not an independent OS thread")?;
    wait(system, || Ok(find(system, busy.id)?.is_some_and(|info| info.switches >= 4)))?;
    memory(&handle, b"observer-owned memory")?;
    require(processes.current()?.id == me.id && processes.current()?.thread == me.thread,
            "scheduler did not restore observer identity")?;
    system.log("PROCESS_OBSERVER_PROGRESS_DURING_SPIN\n")?;
    wait(system, || Ok(find(system, busy.id)?.is_none()))?;
    require(processes.count()? == 1, "returned spinner was not reaped")?;
    system.log("PROCESS_PREEMPTION_PASS\n")?;
    let worker = system.files().lookup(volume, 2, "Applications/ProcessWorker")?;
    let mut prior = None;
    for round in 0..2
    {
        system.scrap().clear()?;
        let mut announcement = crate::LogBuffer::new();
        use core::fmt::Write;
        let _ = write!(announcement, "process-observer-window:{}", own_window.0);
        system.scrap().put_text(announcement.text())?;
        system.events().post(Event { kind: EventKind::Application1,
                                      message: 0x1111aaaa,
                                      ..Default::default() })?;
        let child = processes.launch(volume, worker.id)?;
        require(child.id != me.id && child.thread != me.thread,
                "worker is not an independent OS thread")?;
        if let Some((id, thread)) = prior
        {
            require(child.id != id && child.thread != thread,
                    "relaunch reused a terminated identity")?;
        }
        require(processes.launch(volume, worker.id)?.id == child.id,
                "duplicate launch made another instance")?;
        require(processes.count()? == 2, "live worker count")?;
        let mut child_window = None;
        wait(system, || {
            let mut bytes = [0; 64];
            if let Ok(text) = system.scrap().text(&mut bytes)
            {
                child_window = parse_window(text, "process-worker-window:");
            }
            Ok(child_window.is_some())
        })?;
        menu(system)?;
        let child_window = child_window.ok_or(Failure::Contract("worker window publication"))?;
        require(system.windows().info(child_window).err().is_some_and(|e| e.0 == ACCESS_DENIED),
                "foreign window info was not denied")?;
        require(system.windows().close(child_window).err().is_some_and(|e| e.0 == ACCESS_DENIED),
                "foreign window close was not denied")?;
        let event = system.events()
                          .next(1 << EventKind::Application1 as u16)?
                          .ok_or(Failure::Contract("observer private event missing"))?;
        require(event.message == 0x1111aaaa, "worker private event leaked to observer")?;
        synthetic_private(system, child_window, OBSERVER_SYNTHETIC)?;
        let activation_mask = 1 << EventKind::Activate as u16;
        system.events().flush(activation_mask, 0)?;
        let background =
            system.windows()
                  .create(WindowSpec::new(Rect::new(65, 100, 355, 210), "Background Create"))?;
        require(system.events().next(activation_mask)?.is_none(),
                "background creation transiently activated its own window")?;
        require(state(system, child.id)?.foreground != 0 && processes.current()?.foreground == 0,
                "background creation changed foreground application")?;
        system.windows().close(background)?;
        system.log("PROCESS_BACKGROUND_CREATE_PASS cases=2\n")?;
        system.events().flush(activation_mask, 0)?;
        system.windows().select_desktop()?;
        require(processes.current()?.foreground != 0,
                "background desktop selection did not activate Finder")?;
        require(system.events().next(activation_mask)?.is_none(),
                "background desktop selection transiently activated a Finder window")?;
        system.windows().select_desktop()?;
        require(system.events().next(activation_mask)?.is_none()
                && system.window(0)?.id == own_front.0,
                "repeated desktop selection changed activation or window order")?;
        system.log("PROCESS_BACKGROUND_DESKTOP_PASS cases=3\n")?;
        // Selecting a nonfront window from a background app must activate only
        // that final window, rather than first selecting its former front.
        for selected in [own_window, own_front]
        {
            processes.activate(child.id)?;
            system.events().flush(activation_mask, 0)?;
            system.windows().select(selected)?;
            let event = system.events()
                              .next(activation_mask)?
                              .ok_or(Failure::Contract("missing requested window activation"))?;
            require(event.message == selected.0
                    && event.modifiers & mac_toolbox::ACTIVE_FLAG != 0,
                    "background select activated the previous front window")?;
            require(system.events().next(activation_mask)?.is_none(),
                    "background select emitted intermediate activation")?;
            require(system.window(0)?.id == selected.0 && processes.current()?.foreground != 0,
                    "background requested window did not become foreground")?;
        }
        system.log("PROCESS_BACKGROUND_SELECT_PASS cases=2\n")?;
        atomic_activation(system, me.id, child.id, own_front)?;
        processes.activate(me.id)?;
        menu(system)?;
        require(state(system, me.id)?.foreground != 0 && state(system, child.id)?.foreground == 0,
                "foreground activation")?;
        processes.hide_others()?;
        menu(system)?;
        require(state(system, child.id)?.hidden != 0, "hide others did not hide worker")?;
        processes.activate(child.id)?;
        menu(system)?;
        require(state(system, child.id)?.hidden == 0 && state(system, child.id)?.foreground != 0,
                "activate did not restore hidden worker")?;
        processes.hide(child.id)?;
        menu(system)?;
        require(state(system, child.id)?.hidden != 0 && state(system, me.id)?.foreground != 0,
                "hide did not fall back to observer")?;
        processes.show_all()?;
        menu(system)?;
        require(state(system, child.id)?.hidden == 0 && state(system, me.id)?.foreground != 0,
                "show all changed foreground")?;
        processes.activate(child.id)?;
        menu(system)?;
        memory(&handle, b"observer-owned memory")?;
        resource(system, "process-observer")?;
        require(system.windows().info(own_window)?.id == own_window.0,
                "observer window ownership survived switching")?;
        system.events().flush(activation_mask, 0)?;
        system.log(if round == 0
                   {
                       "PROCESS_READY_FOR_INPUT_1\n"
                   }
                   else
                   {
                       "PROCESS_READY_FOR_INPUT_2\n"
                   })?;
        // Process-list queries and event polling are separate ECALLs. Preserve
        // Activate if exit occurs between a live-process check and the next poll.
        wait_mask(system,
                  || Ok(find(system, child.id)?.is_none()),
                  EVERY_EVENT & !(activation_mask | (1 << EventKind::Application1 as u16)))?;
        let final_activation = system.events()
                                     .next(activation_mask)?
                                     .ok_or(Failure::Contract("missing final exit activation"))?;
        require(final_activation.message == own_front.0
                && final_activation.modifiers & mac_toolbox::ACTIVE_FLAG != 0,
                "multiwindow exit activated an intermediate window")?;
        require(system.events().next(activation_mask)?.is_none(),
                "multiwindow exit emitted extra activation")?;
        system.log("PROCESS_MULTIWINDOW_EXIT_PASS cases=2\n")?;
        menu(system)?;
        require(processes.count()? == 1 && processes.current()?.foreground != 0,
                "worker exit/focus cleanup")?;
        require(processes.activate(child.id).is_err(), "terminated AppId remained activatable")?;
        memory(&handle, b"observer-owned memory")?;
        prior = Some((child.id, child.thread));
    }
    handle.dispose()?;
    system.windows().close(own_front)?;
    system.windows().close(own_window)?;
    system.log("PROCESS_USERSPACE_PASS preemption=passed launch=passed invalid=3 \
                relaunch=passed windows=passed resources=passed memory=passed events=passed \
                focus=passed hide=passed appmenu=passed\n")?;
    Ok(())
}

pub fn worker(system: &System<'_>) -> Result<()>
{
    static ENTERED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    require(!ENTERED.swap(true, core::sync::atomic::Ordering::Relaxed),
            "relaunch did not initialize application BSS")?;
    resource(system, "process-worker")?;
    let identity = system.processes().current()?;
    require(identity.id != 0 && identity.thread != 0, "worker execution identity")?;
    let handle = system.memory().handle_from_bytes(b"worker-owned memory")?;
    let window = system.windows()
                       .create(WindowSpec::new(Rect::new(100, 180, 400, 310), "Process Worker"))?;
    let second_window =
        system.windows().create(WindowSpec::new(Rect::new(120, 200, 420, 330), "Worker Front"))?;
    let mut announcement = [0; 64];
    let parent_window = parse_window(system.scrap().text(&mut announcement)?, "process-observer-window:")
        .ok_or(Failure::Contract("observer window publication"))?;
    synthetic_private(system, parent_window, WORKER_SYNTHETIC)?;
    let mut text = crate::LogBuffer::new();
    use core::fmt::Write;
    let _ = write!(text, "process-worker-window:{}", window.0);
    system.events().post(Event { kind: EventKind::Application1,
                                  message: 0x2222bbbb,
                                  ..Default::default() })?;
    let event = system.events()
                      .next(1 << EventKind::Application1 as u16)?
                      .ok_or(Failure::Contract("worker private event missing"))?;
    require(event.message == 0x2222bbbb, "observer private event leaked to worker")?;
    // The parent announcement has been consumed. Scrap Put appends formats, so
    // replace that handshake flavor explicitly before publishing the child ID.
    system.scrap().clear()?;
    system.scrap().put_text(text.text())?;
    system.log("PROCESS_WORKER_READY\n")?;
    let start = system.events().uptime_ticks()?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        require(event.when != OBSERVER_SYNTHETIC,
                "observer synthetic event reached foreign worker")?;
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 1800,
                "worker input timeout")?;
        if event.kind == EventKind::Update && [window.0, second_window.0].contains(&event.message)
        {
            system.begin_update(event.message)?;
            system.end_update(event.message)?;
        }
        if event.kind == EventKind::KeyDown && event.character() == b'x'
        {
            resource(system, "process-worker")?;
            memory(&handle, b"worker-owned memory")?;
            require(system.processes().current()?.id == identity.id, "worker identity changed")?;
            require(system.windows().info(window)?.id == window.0, "worker owned window lost")?;
            system.log("PROCESS_WORKER_INPUT_PASS\n")?;
            // Deliberately leave window and handle live: exit must reclaim them.
            core::mem::forget(handle);
            return Ok(());
        }
    }
}

/// `clock` reads the hardware time counter; the timed interval makes no trap or
/// cooperative yield, so another app can progress only through real preemption.
pub fn spinner(system: &System<'_>, clock: impl Fn() -> u64) -> Result<()>
{
    resource(system, "process-spinner")?;
    system.log("PROCESS_SPIN_STARTED\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 30_000_000
    {
        core::hint::spin_loop();
    }
    system.log("PROCESS_SPIN_FINISHED\n")?;
    Ok(())
}
