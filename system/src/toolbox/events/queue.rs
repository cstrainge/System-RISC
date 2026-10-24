#![forbid(unsafe_code)]
use mac_abi::{Point, events::*};

pub struct EventManager
{
    queue:         [Event; 128],
    routed:        [bool; 128],
    route_owners:  [u64; 128],
    positions:     [EventPosition; 128],
    len:           usize,
    os_queue:      [Event; 20],
    os_len:        usize,
    os_mask:       u16,
    pub mouse:     Point,
    pub modifiers: u16,
    pub keys:      KeyMap,
    pub dropped:   u32
}
impl Default for EventManager
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl EventManager
{
    pub fn new() -> Self
    {
        Self { queue:        [Event::default(); 128],
               routed:       [false; 128],
               route_owners: [0; 128],
               positions:    [EventPosition::default(); 128],
               len:          0,
               os_queue:     [Event::default(); 20],
               os_len:       0,
               os_mask:      EVERY_EVENT,
               mouse:        Point { h: 16,
                                     v: 444 },
               modifiers:    BTN_STATE,
               keys:         KeyMap::default(),
               dropped:      0 }
    }

    /// Progress is advisory and coalesced by object, not by timer/input event.
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn post_transfer(&mut self, event: Event)
    {
        if let Some(queued) =
            self.queue[..self.len].iter_mut().find(|queued| {
                                                 queued.kind == event.kind
                                                 && queued.message == event.message
                                                 && queued.payload == event.payload
                                                 && queued.window == event.window
                                             })
        {
            *queued = event;
            return;
        }
        self.post(event);
    }

    pub fn post(&mut self, event: Event)
    {
        self.push(event, false);
    }

    /// Coalesce adjacent within notifications, preserving all enter/leave/drop
    /// ordering. Physical motion must not flood a receiver's bounded queue.
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn post_drag(&mut self, event: Event, position: EventPosition)
    {
        if event.message & 0xff00_0000 == mac_abi::drag::WITHIN_EVENT && self.len != 0
        {
            let last = &mut self.queue[self.len - 1];
            if last.kind == event.kind
               && last.message == event.message
               && last.payload == event.payload
               && last.window == event.window
               && !self.routed[self.len - 1]
            {
                *last = event;
                self.positions[self.len - 1] = position;
                return;
            }
        }
        self.post_positioned(event, position);
    }

    pub fn post_positioned(&mut self, event: Event, position: EventPosition)
    {
        self.push(event, false);
        self.positions[self.len - 1] = position;
    }

    /// Only trusted Window Manager/input paths may request owner routing. Public
    /// application-posted events remain in their own queue, even when their
    /// window field names another application's object.
    pub(crate) fn post_system(&mut self, event: Event)
    {
        self.push(event, true);
    }

    /// Capture the validated owner before an object's disposal removes lookup.
    /// Application posting never gets access to this routing metadata.
    #[cfg(any(target_arch = "riscv64", test))]
    pub(crate) fn post_system_for(&mut self, owner: u64, event: Event)
    {
        self.push(event, true);
        self.route_owners[self.len - 1] = owner;
    }

    #[cfg(any(target_arch = "riscv64", test))]
    pub(crate) fn next_system(&mut self) -> Option<(Event, EventPosition, u64)>
    {
        let index = self.routed[..self.len].iter().position(|routed| *routed)?;
        let position = self.positions[index];
        let owner = self.route_owners[index];
        Some((self.remove(index), position, owner))
    }

    fn remove(&mut self, index: usize) -> Event
    {
        let event = self.queue[..self.len][index];
        // A one-event queue and a tail removal have nothing to move. Keep
        // that common input path out of four empty memmove operations.
        if index + 1 < self.len
        {
            self.queue.copy_within(index + 1..self.len, index);
            self.routed.copy_within(index + 1..self.len, index);
            self.route_owners.copy_within(index + 1..self.len, index);
            self.positions.copy_within(index + 1..self.len, index);
        }
        self.len -= 1;
        event
    }

    fn push(&mut self, event: Event, routed: bool)
    {
        if self.len == self.queue.len()
        {
            // Owned progress/document notices are level notifications. Losing
            // the only terminal wake would strand an otherwise finished owner.
            let evict = self.queue[..self.len].iter()
                                              .position(|queued| {
                                                  !(queued.kind
                                                    == mac_abi::events::EventKind::OsEvent
                                                    && matches!(queued.message,
                                                        mac_abi::events::FILE_TRANSFER_CHANGED
                                                        | mac_abi::processes::DOCUMENT_AVAILABLE))
                                              })
                                              .unwrap_or(0);
            self.remove(evict);
            self.dropped = self.dropped.saturating_add(1);
        }
        self.queue[self.len] = event;
        self.positions[self.len] = EventPosition::default();
        self.routed[self.len] = routed;
        self.route_owners[self.len] = 0;
        self.len += 1;
    }

    pub fn next_matching(&mut self, mask: u16) -> Option<Event>
    {
        let index = self.queue[..self.len].iter().position(|event| event.matches(mask))?;
        Some(self.remove(index))
    }

    pub fn peek_matching(&self, mask: u16) -> Option<Event>
    {
        self.queue[..self.len].iter().find(|e| e.matches(mask)).copied()
    }

    pub fn positioned_matching(&mut self, mask: u16, remove: bool)
                               -> Option<(Event, EventPosition)>
    {
        let index = self.queue[..self.len].iter().position(|event| event.matches(mask))?;
        let result = (self.queue[index], self.positions[index]);
        if remove
        {
            self.remove(index);
        }
        Some(result)
    }

    /// Remove matching entries strictly before the first stopping entry.
    /// An entry in both masks is the stop entry and remains queued.
    pub fn flush(&mut self, mask: u16, stop_mask: u16)
    {
        let mut index = 0;
        while index < self.len
        {
            let event = self.queue[index];
            if event.matches(stop_mask)
            {
                break;
            }
            if event.matches(mask)
            {
                self.remove(index);
            }
            else
            {
                index += 1;
            }
        }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn position_last(&mut self, position: EventPosition)
    {
        if self.len != 0
        {
            self.positions[self.len - 1] = position;
        }
    }

    pub fn null_event(&self, when: u32) -> Event
    {
        Event { when,
                where_: self.mouse,
                modifiers: self.modifiers,
                ..Event::default() }
    }

    /// Update physical state and enqueue the low-level hardware event before
    /// window, menu or modal-dialog routing consumes the same input packet.
    pub fn observe_input(&mut self, input: super::input::Input, when: u32)
    {
        use super::input::Input;
        let (kind, message) = match input
        {
            Input::Move(point) =>
            {
                self.mouse = point;
                return;
            }
            Input::Button { point,
                            down } =>
            {
                self.mouse = point;
                if down
                {
                    self.modifiers &= !BTN_STATE;
                }
                else
                {
                    self.modifiers |= BTN_STATE;
                }
                (if down { EventKind::MouseDown } else { EventKind::MouseUp }, 0)
            }
            Input::Key { mac_code,
                         character,
                         down,
                         repeat,
                         modifiers } =>
            {
                self.modifiers = modifiers | (self.modifiers & BTN_STATE);
                if mac_code == 0xff
                {
                    return;
                }
                (if !down
                 {
                     EventKind::KeyUp
                 }
                 else if repeat
                 {
                     EventKind::AutoKey
                 }
                 else
                 {
                     EventKind::KeyDown
                 },
                 (mac_code as u32) << 8 | character as u32)
            }
        };
        self.os_post(kind, message, when);
    }

    fn os_post(&mut self, kind: EventKind, message: u32, when: u32)
    {
        if (1 << kind as u16) & self.os_mask == 0
        {
            return;
        }
        // Captured System 7.6.1 retains the newest 20 events; overflow is a
        // successful post which silently releases the oldest queue element.
        if self.os_len == self.os_queue.len()
        {
            self.os_queue.copy_within(1..self.os_len, 0);
            self.os_len -= 1;
        }
        self.os_queue[self.os_len] = Event { kind,
                                             message,
                                             ..self.null_event(when) };
        self.os_len += 1;
    }

    pub fn service(&mut self, call: &mut EventCall, when: u32, uptime_ticks: u64)
                   -> mac_abi::Status
    {
        use mac_abi::{NOT_SUPPORTED, OK};
        call.present = 0;
        let selected = match call.operation
        {
            NATIVE_POST =>
            {
                self.post(call.event);
                None
            }
            NATIVE_PEEK => self.peek_matching(call.mask),
            NATIVE_FLUSH =>
            {
                self.flush(call.mask, call.stop_mask);
                None
            }
            OS_POST =>
            {
                if !call.event.matches(self.os_mask)
                {
                    return EVENT_NOT_ENABLED;
                }
                self.os_post(call.event.kind, call.event.message, when);
                None
            }
            OS_PEEK | OS_GET =>
            {
                let index = self.os_queue[..self.os_len].iter().position(|e| e.matches(call.mask));
                index.map(|index| {
                         let event = self.os_queue[index];
                         if call.operation == OS_GET
                         {
                             self.os_queue.copy_within(index + 1..self.os_len, index);
                             self.os_len -= 1;
                         }
                         event
                     })
            }
            OS_FLUSH =>
            {
                flush(&mut self.os_queue, &mut self.os_len, call.mask, call.stop_mask);
                None
            }
            INPUT_STATE => None,
            OS_STILL_DOWN => None,
            OS_SET_MASK =>
            {
                self.os_mask = call.mask;
                None
            }
            OS_GET_MASK =>
            {
                call.mask = self.os_mask;
                None
            }
            _ => return NOT_SUPPORTED
        };
        call.queued = if matches!(call.operation, NATIVE_POST | NATIVE_PEEK | NATIVE_FLUSH)
        {
            self.len
        }
        else
        {
            self.os_len
        } as u32;
        call.event = selected.unwrap_or_else(|| self.null_event(when));
        call.present = if call.operation == OS_STILL_DOWN
        {
            (self.modifiers & BTN_STATE == 0
             && self.os_queue[..self.os_len].iter()
                                            .find_map(|event| {
                                                match event.kind
                                                {
                                                    EventKind::MouseDown => Some(true),
                                                    EventKind::MouseUp => Some(false),
                                                    _ => None
                                                }
                                            })
                                            .unwrap_or(true)) as u32
        }
        else
        {
            selected.is_some() as u32
        };
        call.keys = self.keys;
        call.ticks = when;
        call.uptime_ticks = uptime_ticks;
        OK
    }
}

fn flush(queue: &mut [Event], len: &mut usize, mask: u16, stop_mask: u16)
{
    let mut write = 0;
    let mut stopped = false;
    for read in 0..*len
    {
        let event = queue[read];
        stopped |= event.matches(stop_mask);
        if stopped || !event.matches(mask)
        {
            queue[write] = event;
            write += 1;
        }
    }
    *len = write;
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn masks_preserve_unrequested_events_and_wire_layout()
    {
        let mut manager = EventManager::new();
        manager.post(Event { payload:   0,
                             kind:      EventKind::MouseDown,
                             message:   0x12345678,
                             when:      7,
                             where_:    Point { h: -2,
                                                v: 3 },
                             modifiers: SHIFT_KEY,
                             window:    0 });
        manager.post(Event { kind: EventKind::KeyDown,
                             message: 65,
                             ..Event::default() });
        assert_eq!(manager.next_matching(1 << 3).unwrap().character(), 65);
        let event = manager.next_matching(EVERY_EVENT).unwrap();
        assert_eq!(event.to_be_bytes(),
                   [0, 1, 0x12, 0x34, 0x56, 0x78, 0, 0, 0, 7, 0, 3, 255, 254, 2, 0]);
        assert!(manager.next_matching(EVERY_EVENT).is_none());
    }
    #[test]
    fn routing_authority_is_private_and_survives_queue_compaction()
    {
        let mut manager = EventManager::new();
        let event = Event { kind: EventKind::OsEvent,
                            message: mac_abi::WINDOW_CLOSE_REQUEST,
                            window: u64::MAX - 7,
                            payload: u64::MAX - 9,
                            ..Event::default() };
        manager.post(event);
        assert!(manager.next_system().is_none(), "application fields cannot forge trusted routing");
        manager.post_system_for(70001, event);
        manager.post_system_for(70002,
                                Event { window: event.window - 1,
                                        ..event });
        assert_eq!(manager.next_matching(EVERY_EVENT), Some(event));
        let (first, _, owner) = manager.next_system().unwrap();
        assert_eq!((first, owner), (event, 70001));
        let (second, _, owner) = manager.next_system().unwrap();
        assert_eq!((second.window, second.payload, owner),
                   (event.window - 1, event.payload, 70002));
        manager.post_system(event);
        assert_eq!(manager.next_system().unwrap().2,
                   0,
                   "reused queue positions cannot inherit old owner authority");
    }

    #[test]
    fn queue_overflow_is_bounded_and_observable()
    {
        let mut manager = EventManager::new();
        for n in 0..130
        {
            manager.post(Event { kind: EventKind::KeyDown,
                                 message: n,
                                 ..Event::default() });
        }
        assert_eq!(manager.dropped, 2);
        assert_eq!(manager.next_matching(EVERY_EVENT).unwrap().message, 2);
    }
}
