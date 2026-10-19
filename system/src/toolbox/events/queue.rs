#![forbid(unsafe_code)]
use mac_abi::{Point, events::*};

pub struct EventManager
{
    queue:         [Event; 128],
    len:           usize,
    pub mouse:     Point,
    pub modifiers: u16,
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
               mouse:     Point { h: 16,
                                  v: 444 },
               modifiers: BTN_STATE,
               dropped:   0 }
    }

    pub fn post(&mut self, event: Event)
    {
        if self.len == self.queue.len()
        {
            self.queue.copy_within(1..self.len, 0);
            self.len -= 1;
            self.dropped += 1;
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

    pub fn null_event(&self, when: u32) -> Event
    {
        Event { when,
                where_: self.mouse,
                modifiers: self.modifiers,
                ..Event::default() }
    }
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
