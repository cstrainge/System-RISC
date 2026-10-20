//! Foreground event-loop progress, independent of application cursor policy.
#![forbid(unsafe_code)]
pub const STALL_TICKS: u32 = 30; // Half a second at the classic 60 Hz timebase.
#[derive(Default)]
pub struct Responsiveness
{
    app:       u64,
    last_poll: u32,
    active:    bool
}
impl Responsiveness
{
    pub fn active(&self) -> bool
    {
        self.active
    }

    pub fn foreground(&mut self, app: u64, now: u32) -> bool
    {
        if self.app == app
        {
            return false;
        }
        let changed = self.active;
        self.app = app;
        self.last_poll = now;
        self.active = false;
        changed
    }

    pub fn polled(&mut self, app: u64, now: u32) -> bool
    {
        if app != self.app
        {
            return false;
        }
        let changed = self.active;
        self.active = false;
        self.last_poll = now;
        changed
    }

    pub fn tick(&mut self, app: u64, now: u32, waiting: bool) -> bool
    {
        let previous = self.active;
        self.foreground(app, now);
        if waiting
        {
            self.last_poll = now;
        }
        self.active = !waiting && now.wrapping_sub(self.last_poll) >= STALL_TICKS;
        previous != self.active
    }
}
