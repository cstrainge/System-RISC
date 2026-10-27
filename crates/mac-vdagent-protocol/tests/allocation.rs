use std::{alloc::{GlobalAlloc, Layout, System},
          sync::atomic::{AtomicUsize, Ordering}};

use mac_vdagent_protocol::*;

static REFUSE_SIZE: AtomicUsize = AtomicUsize::new(0);
struct Allocator;
// SAFETY: normal allocations/deallocations forward the exact System contract.
// The single refusal models alloc returning null, which Vec::try_reserve handles.
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if REFUSE_SIZE.compare_exchange(layout.size(), 0, Ordering::SeqCst, Ordering::SeqCst)
                      .is_ok()
        {
            return std::ptr::null_mut();
        }
        // SAFETY: same validated GlobalAlloc layout, no ownership changes.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        // SAFETY: each successful allocation was made by System with this layout.
        unsafe {
            System.dealloc(pointer, layout);
        }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

#[test]
fn receive_oom_retains_header_consumption_and_owned_state_for_a_real_retry()
{
    let caps = Capabilities::empty().with(Capability::ClipboardByDemand)
                                    .with(Capability::ClipboardSelection);
    let text = "x".repeat(65536);
    let message = OwnedMessage::text(Selection::Clipboard, &text, caps, Limits::qemu()).unwrap();
    let mut stream = Vec::new();
    for chunk in message.chunks(1024).unwrap()
    {
        for part in chunk.segments()
        {
            stream.extend_from_slice(part);
        }
    }
    let mut decoder = Decoder::new(Limits::qemu());
    REFUSE_SIZE.store(message.header().payload_bytes(), Ordering::SeqCst);
    let failure = decoder.feed(&stream).unwrap_err();
    assert_eq!(failure.consumed, CHUNK_HEADER_BYTES + MESSAGE_HEADER_BYTES);
    assert_eq!(failure.cause, Error::NoMemory { bytes: message.header().payload_bytes() });
    assert!(!decoder.is_poisoned());
    assert_eq!(decoder.retained_capacity(), 0);
    assert_eq!(REFUSE_SIZE.load(Ordering::SeqCst), 0);
    let resumed = decoder.feed(&stream[failure.consumed..]).unwrap();
    assert_eq!(resumed.consumed, stream.len() - failure.consumed);
    let frame = resumed.frame.unwrap();
    assert!(matches!(frame.message(caps).unwrap(),Message::Data(ClipboardData::Text{text:actual,..}) if actual==text));
}
