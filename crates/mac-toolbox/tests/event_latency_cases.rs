//! Canonical native Event Manager wait and delivery assertions on real RV64.
//! Timing samples are observations, not host-dependent pass/fail deadlines.
#![cfg(target_arch = "riscv64")]
use core::fmt::Write;

use mac_toolbox::{System,
                  abi::EventKind,
                  events::classic::{EVERY_EVENT, Event}};
#[derive(Debug)]
pub enum Failure
{
    Contract(&'static str),
    Service(i32)
}
impl From<mac_toolbox::Error> for Failure
{
    fn from(error: mac_toolbox::Error) -> Self
    {
        Self::Service(error.status())
    }
}
impl From<i32> for Failure
{
    fn from(error: i32) -> Self
    {
        Self::Service(error)
    }
}
type Result<T> = core::result::Result<T, Failure>;
fn require(condition: bool, message: &'static str) -> Result<()>
{
    if condition { Ok(()) } else { Err(Failure::Contract(message)) }
}
const APP1: u16 = 1 << EventKind::Application1 as u16;
const APP2: u16 = 1 << EventKind::Application2 as u16;
const KEYS: u16 = (1 << EventKind::KeyDown as u16) | (1 << EventKind::KeyUp as u16);
fn distribution(system: &System<'_>, label: &str) -> Result<()>
{
    let events = system.events();
    let mut samples = [0u64; 64];
    for (index, sample) in samples.iter_mut().enumerate()
    {
        let event = Event { kind: EventKind::Application1,
                            message: index as u32,
                            ..Default::default() };
        let before = events.monotonic_nanoseconds()?;
        events.post(event.into())?;
        require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP1), 60)
                      .map(mac_toolbox::Event::into_record)?
                == event,
                "queued event payload changed")?;
        let after = events.monotonic_nanoseconds()?;
        require(after >= before, "monotonic clock ran backwards")?;
        *sample = after - before;
    }
    samples.sort_unstable();
    let mut text = crate::LogBuffer::new();
    let _ = writeln!(text,
                     "EVENT_LATENCY_DISTRIBUTION \
                      {{\"scenario\":\"{label}\",\"samples\":64,\"p50_ns\":{},\"p95_ns\":{},\"\
                      p99_ns\":{},\"max_ns\":{}}}",
                     samples[31], samples[60], samples[63], samples[63]);
    system.log(text.text())?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP1), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "queued event delivered twice")?;
    Ok(())
}
fn physical_keys(system: &System<'_>, label: &str) -> Result<()>
{
    let events = system.events();
    events.flush(mac_toolbox::events::EventMask::from_bits_retain(KEYS),
                 mac_toolbox::events::EventMask::from_bits_retain(0))?;
    let mut text = crate::LogBuffer::new();
    let _ = writeln!(text, "EVENT_LATENCY_READY_KEYS_{label}");
    system.log(text.text())?;
    for character in b"abcd"
    {
        for kind in [EventKind::KeyDown, EventKind::KeyUp]
        {
            let event = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS),
                                          300)
                              .map(mac_toolbox::Event::into_record)?;
            require(event.kind == kind && event.character() == *character,
                    "physical key was dropped, reordered or duplicated")?;
        }
    }
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "extra physical key delivery")?;
    let mut text = crate::LogBuffer::new();
    let _ = writeln!(text, "EVENT_LATENCY_PHYSICAL_PASS scenario={label} transitions=8");
    system.log(text.text())?;
    Ok(())
}
pub fn run(system: &System<'_>) -> Result<()>
{
    system.log("DESKTOP_READY: public SDK wait and latency fixture\n")?;
    let events = system.events();
    events.flush(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT),
                 mac_toolbox::events::EventMask::from_bits_retain(0))?;
    let posted = Event { kind: EventKind::Application1,
                         message: 0x12345678,
                         when: 9,
                         ..Default::default() };
    events.post(posted.into())?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP2), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "wait consumed a mask-excluded event")?;
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(APP1))
                  .map(|event| event.map(mac_toolbox::Event::into_record))?
            == Some(posted),
            "excluded event disappeared")?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP1), 60)
                  .map(mac_toolbox::Event::into_record)?
            == posted,
            "queued event unavailable to wait")?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP1), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "wait delivered queued event more than once")?;
    let posted_null = Event { payload:   0,
                              kind:      EventKind::Null,
                              message:   0x6e756c6c,
                              when:      9,
                              where_:    mac_toolbox::Point { h: 13,
                                                              v: 17 },
                              modifiers: mac_toolbox::events::classic::SHIFT_KEY };
    events.post(posted_null.into())?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(1
                                                                               << EventKind::Null
                                                                                  as u16),
                              60)
                  .map(mac_toolbox::Event::into_record)?
            == posted_null,
            "queued null record was mistaken for idle and replaced after sleeping")?;
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(1
                                                                         << EventKind::Null
                                                                            as u16))
                  .map(|event| event.map(mac_toolbox::Event::into_record))?
                  .is_none(),
            "queued null record was delivered more than once")?;
    system.log("EVENT_LATENCY_POSTED_NULL_PASS\n")?;
    let before = events.uptime_ticks()?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP1), 2)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "timeout manufactured an application event")?;
    require(events.uptime_ticks()?.saturating_sub(before) >= 2,
            "event wait returned before its timeout")?;
    system.log("EVENT_LATENCY_WAIT_CONTRACT_PASS cases=6\n")?;
    distribution(system, "single-app")?;
    let mouse = events.mouse()?;
    system.log("EVENT_LATENCY_READY_POINTER\n")?;
    let motion = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS), 300)
                       .map(mac_toolbox::Event::into_record)?;
    require(motion.kind == EventKind::Null && motion.where_ != mouse,
            "pointer motion did not wake the wait with updated null state")?;
    system.log("EVENT_LATENCY_POINTER_PASS\n")?;
    physical_keys(system, "IDLE")?;
    system.log("EVENT_LATENCY_READY_MASKED_KEY\n")?;
    let masked_start = events.uptime_ticks()?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP2), 60)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "mask-excluded physical key escaped through wait")?;
    require(events.uptime_ticks()?.saturating_sub(masked_start) >= 60,
            "mask-excluded physical key woke the wait early")?;
    for kind in [EventKind::KeyDown, EventKind::KeyUp]
    {
        let event = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS), 60)
                          .map(mac_toolbox::Event::into_record)?;
        require(event.kind == kind && event.character() == b'z',
                "mask-excluded physical key was lost or reordered")?;
    }
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "mask-excluded physical key replayed")?;
    system.log("EVENT_LATENCY_MASKED_KEY_PASS transitions=2\n")?;

    let processes = system.processes();
    let me = processes.current()?;
    let volume = system.files().volume_at(0)?.id;
    let quickdraw =
        system.files().lookup(volume, mac_toolbox::Cnid(2), "Applications/QuickDraw Test")?;
    processes.launch(volume, quickdraw.id)?;
    // Let the independently scheduled application finish its initial drawing.
    let _ = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP2), 12)
                  .map(mac_toolbox::Event::into_record)?;
    let spinner =
        system.files().lookup(volume, mac_toolbox::Cnid(2), "Applications/ProcessSpinner")?;
    let busy = processes.launch(volume, spinner.id)?;
    processes.activate(me.id)?;
    require(processes.count()? == 3 && busy.thread != me.thread,
            "background contention did not use three independent application threads")?;
    let before_switches = processes.current()?.switches;
    let _ = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP2), 6)
                  .map(mac_toolbox::Event::into_record)?;
    let after_switches = processes.current()?.switches;
    let mut text = crate::LogBuffer::new();
    let _ = writeln!(text,
                     "EVENT_LATENCY_BLOCKED_SAMPLE {{\"timeout_ticks\":6,\"reschedules\":{}}}",
                     after_switches.saturating_sub(before_switches));
    system.log(text.text())?;
    // A 100-ms wait must remove this thread from the runnable rotation. The
    // margin allows timer preemption around the two public process queries;
    // busy polling would schedule this thread roughly200times at500us.
    require(after_switches.saturating_sub(before_switches) <= 8,
            "event wait kept the application busy polling")?;
    distribution(system, "three-apps-spinner")?;
    physical_keys(system, "BUSY")?;
    system.log("EVENT_LATENCY_PROGRESS_DURING_SPIN\n")?;
    // Wait beyond the spinner's three-second interval. Timeout wakes must keep
    // working after a runnable background process returns and retires its stack.
    let _ = events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(APP2), 240)
                  .map(mac_toolbox::Event::into_record)?;
    require(processes.count()? == 2, "finished spinner did not exit")?;
    require(events.wait_event(mac_toolbox::events::EventMask::from_bits_retain(KEYS), 0)
                  .map(mac_toolbox::Event::into_record)?
                  .kind
            == EventKind::Null,
            "physical keys were replayed after process exit")?;
    system.log("EVENT_LATENCY_USERSPACE_PASS\n")?;
    Ok(())
}
