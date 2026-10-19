#![forbid(unsafe_code)]
use mac_abi::{Point, events::*};

pub struct EventManager
{
    queue:         [Event; 128],
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
        Self { queue:     [Event::default(); 128],
               len:       0,
               os_queue:  [Event::default(); 20],
               os_len:    0,
               os_mask:   EVERY_EVENT,
               mouse:     Point { h: 16,
                                  v: 444 },
               modifiers: BTN_STATE,
               keys:      KeyMap::default(),
               dropped:   0 }
    }

    pub fn post(&mut self, event: Event)
    {
        if self.len == self.queue.len()
        {
            self.queue.copy_within(1..self.len, 0);
            self.len -= 1;
            self.dropped = self.dropped.saturating_add(1);
        }
        self.queue[self.len] = event;
        self.len += 1;
    }

    pub fn next_matching(&mut self, mask: u16) -> Option<Event>
    {
        let index = self.queue[..self.len].iter().position(|event| event.matches(mask))?;
        let event = self.queue[index];
        self.queue.copy_within(index + 1..self.len, index);
        self.len -= 1;
        Some(event)
    }

    pub fn peek_matching(&self, mask: u16) -> Option<Event>
    {
        self.queue[..self.len].iter().find(|e| e.matches(mask)).copied()
    }

    /// Remove matching entries strictly before the first stopping entry.
    /// An entry in both masks is the stop entry and remains queued.
    pub fn flush(&mut self, mask: u16, stop_mask: u16)
    {
        flush(&mut self.queue, &mut self.len, mask, stop_mask);
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
        manager.post(Event { kind:      EventKind::MouseDown,
                             message:   0x12345678,
                             when:      7,
                             where_:    Point { h: -2,
                                                v: 3 },
                             modifiers: SHIFT_KEY });
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
