//! SCHD v2: the scheduler lives in the extension.
//!
//! Version 1 handed a stateless policy a filtered list and took back one pick.
//! Here the core reports what happened and the extension keeps its own picture:
//! queues, priorities, placement, which cores are running. Each time a core
//! needs a decision the extension receives every event since its last call,
//! then answers with what that core runs and with requests on other cores.
//!
//! Nothing here names a register, an interrupt or an architecture. A core is
//! an opaque identifier and time is microseconds. Wire labels are not
//! authority: the core validates every decision against live state.
use core::time::Duration;

use crate::{Error, Kind, Result, u32_at, u64_at};
pub const VERSION: u32 = 2;
/// The one policy operation: a batch of events in, a decision out.
pub const DECIDE: u32 = 2;
pub const BATCH_MAGIC: [u8; 4] = *b"SCH2";
pub const DECISION_MAGIC: [u8; 4] = *b"SCD2";
pub const BATCH_HEADER: usize = 64;
pub const EVENT_BYTES: usize = 32;
pub const MAX_EVENTS: usize = 4096;
pub const MAX_BATCH_BYTES: usize = BATCH_HEADER + MAX_EVENTS * EVENT_BYTES;
pub const DECISION_HEADER: usize = 48;
pub const REQUEST_BYTES: usize = 16;
pub const MAX_REQUESTS: usize = 8;
pub const MAX_DECISION_BYTES: usize = DECISION_HEADER + MAX_REQUESTS * REQUEST_BYTES;
/// The batch replaces everything the extension knew: the core could not
/// deliver every event in order, or this is the first batch after takeover.
pub const RESYNC: u32 = 1;

/// The only placement fact the core holds. A thread so marked runs on the
/// boot core and nowhere else; the core refuses a decision that says otherwise.
/// Affinity beyond this is the scheduler's own data.
pub const BOOT_CORE_ONLY: u32 = 1 << 8;
/// In `ThreadAdmitted`: the thread is runnable now, not blocked.
pub const RUNNABLE: u32 = 1 << 9;
/// In `ThreadWoken`: input for the user woke it.
pub const INTERACTIVE: u32 = 1 << 10;
/// In `CoreOnline`: this is the boot core. It cannot be parked.
pub const BOOT_CORE: u32 = 1 << 11;
const KIND_MASK: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event
{
    ProcessCreated
    {
        process: u64
    },
    ProcessExited
    {
        process: u64
    },
    ThreadAdmitted
    {
        thread:         u64,
        process:        u64,
        kind:           Kind,
        boot_core_only: bool,
        runnable:       bool
    },
    ThreadBlocked
    {
        thread: u64, core: u64
    },
    ThreadWoken
    {
        thread: u64, interactive: bool
    },
    ThreadExited
    {
        thread: u64
    },
    /// The thread used its quantum on `core` and is still runnable.
    QuantumExpired
    {
        thread:            u64,
        core:              u64,
        used_microseconds: u64
    },
    BootCoreChanged
    {
        thread: u64, boot_core_only: bool
    },
    CoreOnline
    {
        core: u64, boot: bool
    },
    /// The core has nothing to run and no decision outstanding.
    CoreIdle
    {
        core: u64
    },
    CoreParked
    {
        core: u64
    }
}
impl Event
{
    fn code(self) -> u32
    {
        match self
        {
            Self::ProcessCreated { .. } => 1,
            Self::ProcessExited { .. } => 2,
            Self::ThreadAdmitted { .. } => 3,
            Self::ThreadBlocked { .. } => 4,
            Self::ThreadWoken { .. } => 5,
            Self::ThreadExited { .. } => 6,
            Self::QuantumExpired { .. } => 7,
            Self::BootCoreChanged { .. } => 8,
            Self::CoreOnline { .. } => 9,
            Self::CoreIdle { .. } => 10,
            Self::CoreParked { .. } => 11
        }
    }

    /// (flags, subject, first, second)
    fn fields(self) -> (u32, u64, u64, u64)
    {
        let flag = |on: bool, bit: u32| if on { bit } else { 0 };
        match self
        {
            Self::ProcessCreated { process } | Self::ProcessExited { process } => (0, process, 0, 0),
            Self::ThreadAdmitted { thread,
                                   process,
                                   kind,
                                   boot_core_only,
                                   runnable } =>
            {
                (kind as u32 | flag(boot_core_only, BOOT_CORE_ONLY) | flag(runnable, RUNNABLE),
                 thread,
                 process,
                 0)
            }
            Self::ThreadBlocked { thread,
                                  core } => (0, thread, core, 0),
            Self::ThreadWoken { thread,
                                interactive } => (flag(interactive, INTERACTIVE), thread, 0, 0),
            Self::ThreadExited { thread } => (0, thread, 0, 0),
            Self::QuantumExpired { thread,
                                   core,
                                   used_microseconds } => (0, thread, core, used_microseconds),
            Self::BootCoreChanged { thread,
                                    boot_core_only } =>
            {
                (flag(boot_core_only, BOOT_CORE_ONLY), thread, 0, 0)
            }
            Self::CoreOnline { core,
                               boot } => (flag(boot, BOOT_CORE), core, 0, 0),
            Self::CoreIdle { core } | Self::CoreParked { core } => (0, core, 0, 0)
        }
    }

    pub fn encode(self) -> [u8; EVENT_BYTES]
    {
        let (flags, subject, first, second) = self.fields();
        let mut out = [0; EVENT_BYTES];
        out[0..4].copy_from_slice(&self.code().to_le_bytes());
        out[4..8].copy_from_slice(&flags.to_le_bytes());
        out[8..16].copy_from_slice(&subject.to_le_bytes());
        out[16..24].copy_from_slice(&first.to_le_bytes());
        out[24..32].copy_from_slice(&second.to_le_bytes());
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self>
    {
        let (code, flags) = (u32_at(b, 0)?, u32_at(b, 4)?);
        let (subject, first, second) = (u64_at(b, 8)?, u64_at(b, 16)?, u64_at(b, 24)?);
        let has = |bit: u32| flags & bit != 0;
        Ok(match code
        {
            1 => Self::ProcessCreated { process: subject },
            2 => Self::ProcessExited { process: subject },
            3 =>
            {
                Self::ThreadAdmitted { thread:         subject,
                                       process:        first,
                                       kind:           match flags & KIND_MASK
                                       {
                                           0 => Kind::System,
                                           1 => Kind::Application,
                                           2 => Kind::Module,
                                           _ => return Err(Error::Invalid)
                                       },
                                       boot_core_only: has(BOOT_CORE_ONLY),
                                       runnable:       has(RUNNABLE) }
            }
            4 =>
            {
                Self::ThreadBlocked { thread: subject,
                                      core:   first }
            }
            5 =>
            {
                Self::ThreadWoken { thread:      subject,
                                    interactive: has(INTERACTIVE) }
            }
            6 => Self::ThreadExited { thread: subject },
            7 =>
            {
                Self::QuantumExpired { thread:            subject,
                                       core:              first,
                                       used_microseconds: second }
            }
            8 =>
            {
                Self::BootCoreChanged { thread:         subject,
                                        boot_core_only: has(BOOT_CORE_ONLY) }
            }
            9 =>
            {
                Self::CoreOnline { core: subject,
                                   boot: has(BOOT_CORE) }
            }
            10 => Self::CoreIdle { core: subject },
            11 => Self::CoreParked { core: subject },
            _ => return Err(Error::Invalid)
        })
    }
}

/// What every batch says about the call it arrives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn
{
    pub generation:                   u64,
    /// Checked per-core decision sequence.
    pub round:                        u64,
    /// The core asking. Opaque; never an array index.
    pub core:                         u64,
    pub now_microseconds:             u64,
    /// Global order of the first event here. A gap from the last batch this
    /// core delivered is ordinary: other cores delivered the events between.
    pub first_sequence:               u64,
    pub resync:                       bool,
    pub minimum_quantum_microseconds: u32,
    pub maximum_quantum_microseconds: u32
}

#[derive(Clone, Copy)]
pub struct Batch<'a>
{
    turn:   Turn,
    events: &'a [u8]
}
impl<'a> Batch<'a>
{
    pub fn decode(bytes: &'a [u8]) -> Result<Self>
    {
        if bytes.get(..4) != Some(&BATCH_MAGIC) || u32_at(bytes, 4)? != EVENT_BYTES as u32
        {
            return Err(Error::Version);
        }
        let count = u32_at(bytes, 48)? as usize;
        let flags = u32_at(bytes, 52)?;
        if count > MAX_EVENTS || flags & !RESYNC != 0
        {
            return Err(Error::Limit);
        }
        let turn = Turn { generation:                   u64_at(bytes, 8)?,
                          round:                        u64_at(bytes, 16)?,
                          core:                         u64_at(bytes, 24)?,
                          now_microseconds:             u64_at(bytes, 32)?,
                          first_sequence:               u64_at(bytes, 40)?,
                          resync:                       flags & RESYNC != 0,
                          minimum_quantum_microseconds: u32_at(bytes, 56)?,
                          maximum_quantum_microseconds: u32_at(bytes, 60)? };
        if turn.minimum_quantum_microseconds == 0
           || turn.minimum_quantum_microseconds > turn.maximum_quantum_microseconds
        {
            return Err(Error::Invalid);
        }
        let end = BATCH_HEADER + count * EVENT_BYTES;
        if bytes.len() != end
        {
            return Err(Error::Truncated);
        }
        Ok(Self { turn,
                  events: &bytes[BATCH_HEADER..end] })
    }

    pub fn turn(self) -> Turn
    {
        self.turn
    }

    pub fn len(self) -> usize
    {
        self.events.len() / EVENT_BYTES
    }

    pub fn is_empty(self) -> bool
    {
        self.events.is_empty()
    }

    /// Events in the order they happened.
    pub fn events(self) -> impl Iterator<Item = Result<Event>> + 'a
    {
        self.events.chunks_exact(EVENT_BYTES).map(Event::decode)
    }

    /// This core runs `thread` for `quantum`.
    pub fn run(self, thread: u64, quantum: Duration) -> Result<Decision>
    {
        let quantum = u64::try_from(quantum.as_micros()).map_err(|_| Error::Limit)?;
        if thread == 0
           || quantum < u64::from(self.turn.minimum_quantum_microseconds)
           || quantum > u64::from(self.turn.maximum_quantum_microseconds)
        {
            return Err(Error::Invalid);
        }
        Ok(Decision::new(self.turn, thread, quantum))
    }

    /// This core waits for an event.
    pub fn idle(self) -> Decision
    {
        Decision::new(self.turn, 0, 0)
    }
}

/// Writes a batch. The core's side of `Batch::decode`.
pub fn encode_batch<'a>(turn: Turn, events: &[Event], output: &'a mut [u8]) -> Result<&'a [u8]>
{
    if events.len() > MAX_EVENTS
    {
        return Err(Error::Limit);
    }
    let end = BATCH_HEADER + events.len() * EVENT_BYTES;
    let out = output.get_mut(..end).ok_or(Error::Truncated)?;
    out[0..4].copy_from_slice(&BATCH_MAGIC);
    out[4..8].copy_from_slice(&(EVENT_BYTES as u32).to_le_bytes());
    out[8..16].copy_from_slice(&turn.generation.to_le_bytes());
    out[16..24].copy_from_slice(&turn.round.to_le_bytes());
    out[24..32].copy_from_slice(&turn.core.to_le_bytes());
    out[32..40].copy_from_slice(&turn.now_microseconds.to_le_bytes());
    out[40..48].copy_from_slice(&turn.first_sequence.to_le_bytes());
    out[48..52].copy_from_slice(&(events.len() as u32).to_le_bytes());
    out[52..56].copy_from_slice(&(if turn.resync { RESYNC } else { 0 }).to_le_bytes());
    out[56..60].copy_from_slice(&turn.minimum_quantum_microseconds.to_le_bytes());
    out[60..64].copy_from_slice(&turn.maximum_quantum_microseconds.to_le_bytes());
    for (event, slot) in events.iter().zip(out[BATCH_HEADER..].chunks_exact_mut(EVENT_BYTES))
    {
        slot.copy_from_slice(&event.encode());
    }
    Ok(out)
}

/// Something the scheduler asks of a core other than by running a thread on
/// the one that called. Starting and parking a core is the same kind of
/// request as poking one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request
{
    /// Have that core ask for a decision at its next safe point.
    Signal
    {
        core: u64
    },
    /// Stop what that core is running and have it ask again.
    Preempt
    {
        core: u64
    },
    /// Bring that core up. It reports `CoreOnline` when it can take work.
    Start
    {
        core: u64
    },
    /// Take that core out of service once it runs nothing. It reports
    /// `CoreParked`. The boot core is refused.
    Park
    {
        core: u64
    }
}
impl Request
{
    fn parts(self) -> (u32, u64)
    {
        match self
        {
            Self::Signal { core } => (1, core),
            Self::Preempt { core } => (2, core),
            Self::Start { core } => (3, core),
            Self::Park { core } => (4, core)
        }
    }

    fn decode(b: &[u8]) -> Result<Self>
    {
        let core = u64_at(b, 8)?;
        Ok(match u32_at(b, 0)?
        {
            1 => Self::Signal { core },
            2 => Self::Preempt { core },
            3 => Self::Start { core },
            4 => Self::Park { core },
            _ => return Err(Error::Invalid)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision
{
    generation:           u64,
    round:                u64,
    core:                 u64,
    thread:               u64,
    quantum_microseconds: u64,
    requests:             [Option<Request>; MAX_REQUESTS]
}
impl Decision
{
    fn new(turn: Turn, thread: u64, quantum_microseconds: u64) -> Self
    {
        Self { generation: turn.generation,
               round: turn.round,
               core: turn.core,
               thread,
               quantum_microseconds,
               requests: [None; MAX_REQUESTS] }
    }

    /// Adds a request on another core. Refused when the decision is full; a
    /// scheduler with more to ask signals itself and asks on its next turn.
    pub fn request(mut self, request: Request) -> Result<Self>
    {
        let slot = self.requests.iter_mut().find(|slot| slot.is_none()).ok_or(Error::Limit)?;
        *slot = Some(request);
        Ok(self)
    }

    pub fn generation(self) -> u64
    {
        self.generation
    }

    pub fn round(self) -> u64
    {
        self.round
    }

    pub fn core(self) -> u64
    {
        self.core
    }

    /// `None` idles the core.
    pub fn thread(self) -> Option<u64>
    {
        (self.thread != 0).then_some(self.thread)
    }

    pub fn quantum(self) -> Duration
    {
        Duration::from_micros(self.quantum_microseconds)
    }

    pub fn requests(&self) -> impl Iterator<Item = Request> + '_
    {
        self.requests.iter().flatten().copied()
    }

    pub fn encode<'a>(&self, output: &'a mut [u8]) -> Result<&'a [u8]>
    {
        let count = self.requests().count();
        let end = DECISION_HEADER + count * REQUEST_BYTES;
        let out = output.get_mut(..end).ok_or(Error::Truncated)?;
        out.fill(0);
        out[0..4].copy_from_slice(&DECISION_MAGIC);
        out[4..8].copy_from_slice(&(count as u32).to_le_bytes());
        out[8..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..24].copy_from_slice(&self.round.to_le_bytes());
        out[24..32].copy_from_slice(&self.core.to_le_bytes());
        out[32..40].copy_from_slice(&self.thread.to_le_bytes());
        out[40..48].copy_from_slice(&self.quantum_microseconds.to_le_bytes());
        for (request, slot) in
            self.requests().zip(out[DECISION_HEADER..].chunks_exact_mut(REQUEST_BYTES))
        {
            let (code, core) = request.parts();
            slot[0..4].copy_from_slice(&code.to_le_bytes());
            slot[8..16].copy_from_slice(&core.to_le_bytes());
        }
        Ok(out)
    }

    /// The core's side: a decision is accepted only for the turn it answers.
    pub fn decode(turn: Turn, bytes: &[u8]) -> Result<Self>
    {
        if bytes.get(..4) != Some(&DECISION_MAGIC)
        {
            return Err(Error::Version);
        }
        let count = u32_at(bytes, 4)? as usize;
        if count > MAX_REQUESTS
        {
            return Err(Error::Limit);
        }
        if bytes.len() != DECISION_HEADER + count * REQUEST_BYTES
        {
            return Err(Error::Truncated);
        }
        let mut decision = Self { generation:           u64_at(bytes, 8)?,
                                  round:                u64_at(bytes, 16)?,
                                  core:                 u64_at(bytes, 24)?,
                                  thread:               u64_at(bytes, 32)?,
                                  quantum_microseconds: u64_at(bytes, 40)?,
                                  requests:             [None; MAX_REQUESTS] };
        if decision.generation != turn.generation
           || decision.round != turn.round
           || decision.core != turn.core
        {
            return Err(Error::WrongRound);
        }
        let quantum = decision.quantum_microseconds;
        if (decision.thread == 0) != (quantum == 0)
           || (decision.thread != 0
               && (quantum < u64::from(turn.minimum_quantum_microseconds)
                   || quantum > u64::from(turn.maximum_quantum_microseconds)))
        {
            return Err(Error::Invalid);
        }
        for (slot, raw) in
            decision.requests.iter_mut().zip(bytes[DECISION_HEADER..].chunks_exact(REQUEST_BYTES))
        {
            *slot = Some(Request::decode(raw)?);
        }
        Ok(decision)
    }
}
