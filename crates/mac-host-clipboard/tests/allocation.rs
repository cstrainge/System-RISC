use std::{alloc::{GlobalAlloc, Layout, System},
          cell::Cell};

use mac_host_clipboard::{Config, Content, Effect, Engine, Error, Phase, Snapshot};
use mac_vdagent_protocol::{Capabilities, Capability, Decoder, Format, Frame, Limits, OwnedMessage,
                           Selection};

thread_local! { static REFUSE:Cell<bool>=const { Cell::new(false) };static ATTEMPTS:Cell<usize>=const { Cell::new(0) }; }
struct Allocator;
fn refused() -> bool
{
    REFUSE.try_with(|mode| {
              if mode.get()
              {
                  let _ = ATTEMPTS.try_with(|count| count.set(count.get() + 1));
                  true
              }
              else
              {
                  false
              }
          })
          .unwrap_or(false)
}
// SAFETY: every admitted pointer/layout is delegated unchanged to std's
// allocator. Refusal is selected only on the current test thread.
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if refused() { core::ptr::null_mut() } else { unsafe { System.alloc(layout) } }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8
    {
        if refused()
        {
            core::ptr::null_mut()
        }
        else
        {
            unsafe { System.realloc(pointer, layout, bytes) }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
struct Refusal;
impl Drop for Refusal
{
    fn drop(&mut self)
    {
        REFUSE.with(|mode| mode.set(false));
    }
}
fn refuse<R>(run: impl FnOnce() -> R) -> (R, usize)
{
    ATTEMPTS.with(|count| count.set(0));
    REFUSE.with(|mode| mode.set(true));
    let guard = Refusal;
    let result = run();
    drop(guard);
    (result, ATTEMPTS.with(Cell::get))
}
struct Pin
{
    revision: u64,
    text:     String
}
impl Snapshot for Pin
{
    fn revision(&self) -> u64
    {
        self.revision
    }

    fn content(&self) -> Content<'_>
    {
        Content::Text(&self.text)
    }
}
fn pin(revision: u64, text: &str) -> Pin
{
    Pin { revision,
          text: text.into() }
}
fn caps() -> Capabilities
{
    Capabilities::empty().with(Capability::ClipboardByDemand)
                         .with(Capability::ClipboardSelection)
                         .with(Capability::ClipboardGrabSerial)
}
fn frame<R>(message: OwnedMessage, run: impl FnOnce(&Frame<'_>) -> R) -> R
{
    let mut bytes = Vec::new();
    for chunk in message.chunks(1024).unwrap()
    {
        for part in chunk.segments()
        {
            bytes.extend_from_slice(part);
        }
    }
    let mut decoder = Decoder::new(Limits::qemu());
    let result = decoder.feed(&bytes).unwrap();
    run(&result.frame.unwrap())
}
fn drain(engine: &mut Engine<Pin>)
{
    while let Some(value) = engine.output()
    {
        let token = value.token;
        let count = value.remaining_bytes();
        engine.accepted(token, count).unwrap();
    }
}
fn ready() -> Engine<Pin>
{
    let mut engine = Engine::new(Config::qemu());
    engine.open_after_transport_reset(pin(1, "original")).unwrap();
    drain(&mut engine);
    frame(OwnedMessage::announce(caps(), false, Limits::qemu()).unwrap(), |frame| {
        engine.receive(engine.epoch(), frame).unwrap();
    });
    engine
}
#[test]
fn failed_offer_and_response_admission_retry_without_losing_snapshot_or_wire_position()
{
    let mut engine = ready();
    let epoch = engine.epoch();
    let snapshot = pin(2, "pinned Ω");
    let (result, count) = refuse(|| engine.observe(snapshot));
    assert!(matches!(result, Err(Error::Output(mac_vdagent_protocol::Error::NoMemory { .. }))));
    assert_eq!(count, 1);
    assert_eq!(engine.local_revision(), Some(2));
    assert_eq!(engine.queued_messages(), 0);
    engine.poll(epoch).unwrap();
    drain(&mut engine);
    frame(OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),Limits::qemu()).unwrap(),|frame| {
        let (result,count)=refuse(||engine.receive(epoch,frame));
        assert!(matches!(result,Err(Error::Output(mac_vdagent_protocol::Error::NoMemory { .. }))));assert_eq!(count,1);
        assert_eq!(engine.local_revision(),Some(2));assert_eq!(engine.queued_messages(),0);
        engine.receive(epoch,frame).unwrap();assert_eq!(engine.queued_messages(),1);
    });
    drain(&mut engine);
    let (result, count) = refuse(|| engine.poll(epoch));
    assert!(result.is_ok());
    assert_eq!(count, 0, "unchanged idle work must not allocate");
}
#[test]
fn failed_fetch_admission_preserves_offer_retry_and_borrowed_import_needs_no_engine_allocation()
{
    let mut engine = ready();
    let epoch = engine.epoch();
    frame(OwnedMessage::grab(Selection::Clipboard,
                             Some(1),
                             &[Format::UTF8_TEXT],
                             caps(),
                             Limits::qemu()).unwrap(),
          |frame| {
              let (result, count) = refuse(|| engine.receive(epoch, frame));
              assert!(matches!(result,
                               Err(Error::Output(mac_vdagent_protocol::Error::NoMemory { .. }))));
              assert_eq!(count, 1);
              assert!(engine.pending_fetch().is_none());
              assert_eq!(engine.phase(), Phase::Ready);
              engine.receive(epoch, frame).unwrap();
          });
    drain(&mut engine);
    let committed = pin(2, "incoming 🦀");
    frame(OwnedMessage::text(Selection::Clipboard, "incoming 🦀", caps(), Limits::qemu()).unwrap(),
          |frame| {
              let (result, count) = refuse(|| {
                  let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
                  else
                  {
                      panic!()
                  };
                  engine.complete_import(import, Ok::<_, &str>(committed))
              });
              assert!(result.is_ok());
              assert_eq!(count, 0,
                         "the engine must borrow decoder text and consume the resident pin \
                          without another copy");
          });
    assert_eq!(engine.local_revision(), Some(2));
    assert_eq!(engine.queued_messages(), 0);
}
#[test]
fn unchanged_capability_requests_and_duplicate_response_refusals_do_not_allocate()
{
    let mut engine = ready();
    let epoch = engine.epoch();
    engine.observe(pin(2, "text")).unwrap();
    drain(&mut engine);
    let request = || {
        OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),Limits::qemu()).unwrap()
    };
    frame(request(), |frame| {
        engine.receive(epoch, frame).unwrap();
    });
    frame(request(), |frame| {
        let (result, count) = refuse(|| engine.receive(epoch, frame));
        assert!(matches!(result, Err(Error::ResponsePending)));
        assert_eq!(count, 0);
    });
    drain(&mut engine);
    for _ in 0..16
    {
        frame(OwnedMessage::announce(caps(), true, Limits::qemu()).unwrap(), |frame| {
            let (result, count) = refuse(|| engine.receive(epoch, frame));
            assert!(matches!(result, Ok(Effect::Ignored)));
            assert_eq!(count, 0);
        });
    }
    assert_eq!(engine.queued_messages(), 0);
}

#[test]
fn failed_release_allocation_never_exposes_a_new_snapshot_that_failed_text_policy()
{
    let mut engine = ready();
    let epoch = engine.epoch();
    engine.observe(pin(2, "advertised")).unwrap();
    drain(&mut engine);
    let invalid = pin(3, "local\0value");
    let (result, count) = refuse(|| engine.observe(invalid));
    assert!(matches!(result, Err(Error::Output(mac_vdagent_protocol::Error::NoMemory { .. }))));
    assert_eq!(count, 1);
    frame(OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),Limits::qemu()).unwrap(),|frame| { engine.receive(epoch,frame).unwrap(); });
    let mut response = Vec::new();
    while let Some(output) = engine.output()
    {
        let token = output.token;
        let bytes = output.remaining_bytes();
        for segment in output.segments
        {
            response.extend_from_slice(segment);
        }
        engine.accepted(token, bytes).unwrap();
    }
    let mut decoder = Decoder::new(Limits::qemu());
    let result = decoder.feed(&response).unwrap();
    assert!(matches!(result.frame.unwrap().message(caps()).unwrap(),mac_vdagent_protocol::Message::Data(mac_vdagent_protocol::ClipboardData::Unavailable { .. })));
    assert_eq!(engine.local_revision(), Some(3));
    assert_eq!(engine.poll(epoch), Err(Error::EmbeddedNul { offset: 5 }));
}
