//! Event Manager client bindings; the event queue and input decoding are resident.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::events::*;
pub use mac_abi::{Point, events::KeyMap};
mod decoded;
pub use decoded::{Event, EventMask, Input, Key, Modifiers};
/// Original EventRecord and low-level queue vocabulary.
pub mod classic
{
    pub use mac_abi::events::*;
}
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Events<'a>
{
    client: Client<'a>
}
impl<'a> Events<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// Nanoseconds since machine startup, independent of the wall clock.
    /// Current RV64 hardware precision is 100 ns; the value saturates at u64::MAX.
    pub fn monotonic_nanoseconds(self) -> Result<u64>
    {
        self.client.monotonic_nanoseconds()
    }

    pub fn next(self, mask: EventMask) -> Result<Event<'a>>
    {
        self.client.next_event(mask.bits()).map(|event| Event::decode(self.client, event))
    }

    /// Sleep this OS thread until a matching event is ready or the maximum
    /// number of 60-Hz uptime ticks elapses. Zero polls without sleeping.
    /// Physical pointer motion may also return a null event for drag tracking.
    /// Other application threads remain runnable while this thread waits.
    pub fn wait_event(self, mask: EventMask, timeout_ticks: u32) -> Result<Event<'a>>
    {
        self.client
            .wait_event(mask.bits(), timeout_ticks)
            .map(|event| Event::decode(self.client, event))
    }

    /// Post to the native application queue consumed by `next`.
    /// This queue retains 128 events and drops its oldest event on overflow.
    /// It is a native service, separate from the classic OS-level queue.
    pub fn post(self, event: Event<'_>) -> Result<()>
    {
        self.call(EventCall { operation: NATIVE_POST,
                              event: event.into_record(),
                              ..Default::default() })
            .map(|_| ())
    }

    /// Inspect the native queue without removing its first matching event.
    pub fn peek(self, mask: EventMask) -> Result<Option<Event<'a>>>
    {
        self.selected(NATIVE_PEEK, mask.bits()).map(|event| {
                                                   event.map(|event| {
                                                            Event::decode(self.client, event)
                                                        })
                                               })
    }

    /// Remove matching native queued events before the first stop-mask event.
    pub fn flush(self, mask: EventMask, stop_mask: EventMask) -> Result<()>
    {
        self.call(EventCall { operation: NATIVE_FLUSH,
                              mask: mask.bits(),
                              stop_mask: stop_mask.bits(),
                              ..Default::default() })
            .map(|_| ())
    }

    /// Post a kind and message to the classic low-level OS event queue.
    /// Time, position and modifiers come from current physical input state.
    pub fn os_post(self, kind: EventKind, message: u32) -> Result<()>
    {
        self.call(EventCall { operation: OS_POST,
                              event: classic::Event { kind,
                                                      message,
                                                      ..Default::default() },
                              ..Default::default() })
            .map(|_| ())
    }

    pub fn os_peek(self, mask: u16) -> Result<Option<classic::Event>>
    {
        self.selected(OS_PEEK, mask)
    }

    pub fn os_get(self, mask: u16) -> Result<Option<classic::Event>>
    {
        self.selected(OS_GET, mask)
    }

    /// Low-level polling keeps the complete null record when no event matches.
    /// A queued event with kindNull still reports `available = true`.
    pub fn os_event(self, mask: u16, remove: bool) -> Result<EventRead>
    {
        let call = self.call(EventCall { operation: if remove { OS_GET } else { OS_PEEK },
                                         mask,
                                         ..Default::default() })?;
        Ok(EventRead { available: call.present != 0,
                       event:     call.event })
    }

    pub fn os_flush(self, mask: u16, stop_mask: u16) -> Result<()>
    {
        self.call(EventCall { operation: OS_FLUSH,
                              mask,
                              stop_mask,
                              ..Default::default() })
            .map(|_| ())
    }

    pub fn set_os_mask(self, mask: u16) -> Result<()>
    {
        self.call(EventCall { operation: OS_SET_MASK,
                              mask,
                              ..Default::default() })
            .map(|_| ())
    }

    pub fn os_mask(self) -> Result<u16>
    {
        self.call(EventCall { operation: OS_GET_MASK,
                              ..Default::default() })
            .map(|call| call.mask)
    }

    pub fn input_state(self) -> Result<InputState>
    {
        let call = self.call(EventCall { operation: INPUT_STATE,
                                         ..Default::default() })?;
        Ok(InputState { mouse:        call.event.where_,
                        modifiers:    Modifiers::from_bits_retain(call.event.modifiers),
                        keys:         call.keys,
                        ticks:        call.ticks,
                        uptime_ticks: call.uptime_ticks })
    }

    pub fn keys(self) -> Result<KeyMap>
    {
        self.input_state().map(|state| state.keys)
    }

    pub fn mouse(self) -> Result<Point>
    {
        self.input_state().map(|state| state.mouse)
    }

    pub fn button(self) -> Result<bool>
    {
        self.input_state().map(|state| state.modifiers.button_down())
    }

    /// Inspect the first queued low-level mouse event without removing it.
    /// A released physical button always reports false. While it is held, the
    /// first queued mouse-up reports false; a mouse-down or no mouse event
    /// reports true.
    pub fn still_down(self) -> Result<bool>
    {
        self.call(EventCall { operation: OS_STILL_DOWN,
                              ..Default::default() })
            .map(|call| call.present != 0)
    }

    pub fn ticks(self) -> Result<u32>
    {
        self.input_state().map(|state| state.ticks)
    }

    pub fn uptime_ticks(self) -> Result<u64>
    {
        self.input_state().map(|state| state.uptime_ticks)
    }

    fn selected(self, operation: u32, mask: u16) -> Result<Option<classic::Event>>
    {
        let call = self.call(EventCall { operation,
                                         mask,
                                         ..Default::default() })?;
        Ok(if call.present != 0 { Some(call.event) } else { None })
    }

    fn call(self, mut call: EventCall) -> Result<EventCall>
    {
        self.client.event(&mut call)?;
        Ok(call)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputState
{
    /// Global screen coordinates, independent of the current graphics port.
    pub mouse:        Point,
    pub modifiers:    Modifiers,
    pub keys:         KeyMap,
    pub ticks:        u32,
    /// Native extension: nonwrapping 60 Hz ticks, independent of the wall clock.
    pub uptime_ticks: u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventRead
{
    pub available: bool,
    pub event:     classic::Event
}

impl<'a> Events<'a>
{
    /// Await a drag-data future while dispatching this application's ordinary
    /// events. A source owner handles DataRequested by service_promises(); the
    /// receive side wakes on DataReady rather than spinning/polling every tick.
    pub fn resolve_drag<T>(self,
                           future: impl core::future::Future<Output = toolbox_drag::Result<T>>,
                           mut dispatch: impl FnMut(Event<'a>) -> Result<()>)
                           -> toolbox_drag::Result<T>
    {
        use core::{pin::pin,
                   task::{Context, Poll, Waker}};
        let mut future = pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        let mut ready = true;
        loop
        {
            if ready && let Poll::Ready(result) = future.as_mut().poll(&mut context)
            {
                return result;
            }
            let event =
                self.wait_event(EventMask::ALL, 120).map_err(toolbox_drag::DragError::from)?;
            // This compatibility pump serves drag-data futures. Ordinary input
            // and timeout nulls are dispatch work, never data readiness.
            ready = matches!(event, Event::Drag { offer: toolbox_drag::DragEvent::DataReady
                | toolbox_drag::DragEvent::Ended { .. }
                | toolbox_drag::DragEvent::Tracking(toolbox_drag::TrackingOffer { phase: toolbox_drag::TrackPhase::Left, .. }), .. });
            dispatch(event).map_err(toolbox_drag::DragError::from)?;
        }
    }
}
