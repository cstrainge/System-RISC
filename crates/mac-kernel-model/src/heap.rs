//! Real buddy-backed subpage allocation with external, owner-charged metadata.
//! No application/object IDs or raw pointer operations live here. The resident
//! binding must anchor the extent and wrap reservations in its actual HeapArc
//! rollback/quarantine owner before publishing an application allocation.
#![forbid(unsafe_code)]
use alloc::{collections::TryReserveError, sync::Arc, vec::Vec};
use core::{alloc::Layout, fmt, num::NonZeroUsize};

use mac_object_table::shared::InvocationGate;
use spinning_top::Spinlock;

use crate::{account::Committed, buddy::PageBlock, policy::MemoryClass};

const LEVELS: usize = usize::BITS as usize;
/// Pure private container coordinate, not an issuer, generation or resource ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Position(NonZeroUsize);
impl Position
{
    fn at(level: usize, slot: usize) -> Result<Self, Error>
    {
        if level >= LEVELS
        {
            return Err(Error::MetadataLayout);
        }
        let base = 1usize.checked_shl(level as u32).ok_or(Error::MetadataLayout)?;
        if slot >= base
        {
            return Err(Error::MetadataLayout);
        }
        NonZeroUsize::new(base.checked_add(slot).ok_or(Error::MetadataLayout)?)
            .map(Self).ok_or(Error::MetadataLayout)
    }

    fn parts(self) -> (usize, usize)
    {
        let value = self.0.get();
        let level = value.ilog2() as usize;
        (level, value - (1usize << level))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind
{
    Vacant,
    Free,
    Reserved(Layout),
    Live(Layout)
}
#[derive(Clone, Copy)]
struct Node
{
    offset:        usize,
    bytes:         usize,
    previous:      Option<Position>,
    next:          Option<Position>,
    free_previous: Option<Position>,
    free_next:     Option<Position>,
    kind:          Kind
}
impl Node
{
    fn vacant(next: Option<Position>) -> Self
    {
        Self { offset:        0,
               bytes:         0,
               previous:      None,
               next:          None,
               free_previous: None,
               free_next:     next,
               kind:          Kind::Vacant }
    }

    fn segment(offset: usize, bytes: usize, kind: Kind, previous: Option<Position>,
               next: Option<Position>)
               -> Self
    {
        Self { offset,
               bytes,
               previous,
               next,
               free_previous: None,
               free_next: None,
               kind }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Layout,
    MetadataLayout,
    MetadataSuperseded
    {
        level:     usize,
        installed: usize
    },
    MetadataPosition,
    Topology,
    State,
    Counters,
    Retiring,
    NotEmpty,
    Quarantined,
    NeedMetadata
    {
        level: usize,
        slots: usize
    },
    NoFit
    {
        requested:         usize,
        alignment:         usize,
        free_bytes:        usize,
        largest_candidate: usize
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::NeedMetadata { level,
                                 slots } =>
            {
                write!(f,
                       "application extent needs prepared metadata level {level} ({slots} nodes)")
            }
            Self::NoFit { requested,
                          alignment,
                          free_bytes,
                          largest_candidate } =>
            {
                write!(f,
                       "application extent has no fit for {requested} bytes aligned {alignment}: \
                        {free_bytes} free bytes, largest aligned candidate {largest_candidate}")
            }
            cause => write!(f, "application subpage state refused: {cause:?}")
        }
    }
}
#[derive(Debug)]
pub enum MetadataError
{
    Layout,
    Allocation
    {
        level: usize,
        slots: usize,
        cause: TryReserveError
    }
}
impl fmt::Display for MetadataError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Layout => f.write_str("application subpage metadata layout is not representable"),
            Self::Allocation { level,
                               slots,
                               cause } =>
            {
                write!(f,
                       "application subpage metadata level {level} ({slots} nodes) refused: \
                        {cause}")
            }
        }
    }
}
/// Completely initialized outside all allocator/ledger guards under the real
/// owner's Kernel selection. Its immutable capacity is never grown in place.
pub struct PreparedNodes
{
    level: usize,
    nodes: Vec<Node>
}
impl PreparedNodes
{
    pub fn prepare(level: usize) -> Result<Self, MetadataError>
    {
        if level >= LEVELS
        {
            return Err(MetadataError::Layout);
        }
        let slots = 1usize.checked_shl(level as u32).ok_or(MetadataError::Layout)?;
        Layout::array::<Node>(slots).map_err(|_| MetadataError::Layout)?;
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(slots).map_err(|cause| {
                                           MetadataError::Allocation { level,
                                                                       slots,
                                                                       cause }
                                       })?;
        for slot in 0..slots
        {
            let next = if slot + 1 < slots
            {
                Some(Position::at(level, slot + 1).map_err(|_| MetadataError::Layout)?)
            }
            else
            {
                None
            };
            nodes.push(Node::vacant(next));
        }
        Ok(Self { level,
                  nodes })
    }
}
struct Arena
{
    blocks: [Option<Vec<Node>>; LEVELS],
    levels: usize,
    spare:  Option<Position>,
    spares: usize
}
impl Arena
{
    fn new() -> Self
    {
        Self { blocks: core::array::from_fn(|_| None),
               levels: 0,
               spare:  None,
               spares: 0 }
    }

    fn read(&self, p: Position) -> Result<Node, Error>
    {
        let (level, slot) = p.parts();
        self.blocks
            .get(level)
            .and_then(Option::as_ref)
            .and_then(|nodes| nodes.get(slot))
            .copied()
            .ok_or(Error::MetadataPosition)
    }

    fn write(&mut self, p: Position, node: Node) -> Result<(), Error>
    {
        let (level, slot) = p.parts();
        let entry = self.blocks
                        .get_mut(level)
                        .and_then(Option::as_mut)
                        .and_then(|nodes| nodes.get_mut(slot))
                        .ok_or(Error::MetadataPosition)?;
        *entry = node;
        Ok(())
    }

    fn install(&mut self, mut prepared: PreparedNodes) -> Result<(), (Error, PreparedNodes)>
    {
        let valid = (|| {
            if prepared.level != self.levels
               || prepared.level >= LEVELS
               || self.blocks.get(prepared.level).is_none_or(Option::is_some)
               || prepared.nodes.len()
                  != 1usize.checked_shl(prepared.level as u32).ok_or(Error::MetadataLayout)?
            {
                return Err(Error::MetadataLayout);
            }
            let total = self.spares.checked_add(prepared.nodes.len()).ok_or(Error::Counters)?;
            let first = Position::at(prepared.level, 0)?;
            Ok((total, first))
        })();
        let (total, first) = match valid
        {
            Ok(v) => v,
            Err(cause) => return Err((cause, prepared))
        };
        let Some(last) = prepared.nodes.last_mut()
        else
        {
            return Err((Error::MetadataLayout, prepared));
        };
        last.free_next = self.spare;
        let Some(destination) = self.blocks.get_mut(prepared.level)
        else
        {
            return Err((Error::MetadataPosition, prepared));
        };
        // The destination is proven empty. Moving a Vec descriptor neither
        // relocates nodes nor destroys an old owner while the guard is held.
        *destination = Some(prepared.nodes);
        self.levels += 1;
        self.spare = Some(first);
        self.spares = total;
        Ok(())
    }

    fn take(&mut self) -> Result<Position, Error>
    {
        let position = self.spare.ok_or(Error::MetadataPosition)?;
        let node = self.read(position)?;
        if node.kind != Kind::Vacant || self.spares == 0
        {
            return Err(Error::State);
        }
        self.spare = node.free_next;
        self.spares -= 1;
        self.write(position, Node::vacant(None))?;
        Ok(position)
    }

    fn put(&mut self, position: Position) -> Result<(), Error>
    {
        let total = self.spares.checked_add(1).ok_or(Error::Counters)?;
        self.write(position, Node::vacant(self.spare))?;
        self.spare = Some(position);
        self.spares = total;
        Ok(())
    }

    fn requirement(&self) -> Result<Error, Error>
    {
        Ok(Error::NeedMetadata { level: self.levels,
                                 slots: 1usize.checked_shl(self.levels as u32)
                                              .ok_or(Error::MetadataLayout)? })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Active,
    Retiring,
    Quarantined
}
struct ReleasePlan
{
    node:       Node,
    free_bytes: usize,
    reserved:   usize,
    live:       usize
}
struct State
{
    arena:      Arena,
    head:       Position,
    free:       Option<Position>,
    phase:      Phase,
    free_bytes: usize,
    reserved:   usize,
    live:       usize
}
impl State
{
    fn active(&self) -> Result<(), Error>
    {
        match self.phase
        {
            Phase::Active => Ok(()),
            Phase::Retiring => Err(Error::Retiring),
            Phase::Quarantined => Err(Error::Quarantined)
        }
    }

    fn validate_neighbors(&self, p: Position, node: Node) -> Result<(), Error>
    {
        if node.bytes == 0
        {
            return Err(Error::Topology);
        }
        if let Some(previous) = node.previous
        {
            let other = self.arena.read(previous)?;
            if other.next != Some(p) || other.offset.checked_add(other.bytes) != Some(node.offset)
            {
                return Err(Error::Topology);
            }
        }
        else if self.head != p || node.offset != 0
        {
            return Err(Error::Topology);
        }
        if let Some(next) = node.next
        {
            let other = self.arena.read(next)?;
            if other.previous != Some(p)
               || node.offset.checked_add(node.bytes) != Some(other.offset)
            {
                return Err(Error::Topology);
            }
        }
        Ok(())
    }

    fn validate_free(&self, p: Position, node: Node) -> Result<(), Error>
    {
        if node.kind != Kind::Free
        {
            return Err(Error::State);
        }
        if let Some(previous) = node.free_previous
        {
            let other = self.arena.read(previous)?;
            if other.kind != Kind::Free || other.free_next != Some(p)
            {
                return Err(Error::Topology);
            }
        }
        else if self.free != Some(p)
        {
            return Err(Error::Topology);
        }
        if let Some(next) = node.free_next
        {
            let other = self.arena.read(next)?;
            if other.kind != Kind::Free || other.free_previous != Some(p)
            {
                return Err(Error::Topology);
            }
        }
        Ok(())
    }

    fn unlink_free(&mut self, p: Position) -> Result<(), Error>
    {
        let mut node = self.arena.read(p)?;
        self.validate_free(p, node)?;
        if let Some(previous) = node.free_previous
        {
            let mut other = self.arena.read(previous)?;
            other.free_next = node.free_next;
            self.arena.write(previous, other)?;
        }
        else
        {
            self.free = node.free_next;
        }
        if let Some(next) = node.free_next
        {
            let mut other = self.arena.read(next)?;
            other.free_previous = node.free_previous;
            self.arena.write(next, other)?;
        }
        node.free_previous = None;
        node.free_next = None;
        self.arena.write(p, node)
    }

    fn push_free(&mut self, p: Position) -> Result<(), Error>
    {
        let mut node = self.arena.read(p)?;
        if node.kind != Kind::Free || node.free_previous.is_some() || node.free_next.is_some()
        {
            return Err(Error::State);
        }
        if let Some(head) = self.free
        {
            let mut other = self.arena.read(head)?;
            if other.kind != Kind::Free || other.free_previous.is_some()
            {
                return Err(Error::Topology);
            }
            other.free_previous = Some(p);
            self.arena.write(head, other)?;
        }
        node.free_next = self.free;
        self.arena.write(p, node)?;
        self.free = Some(p);
        Ok(())
    }

    fn reserve(&mut self, base: usize, capacity: usize, layout: Layout) -> Result<Position, Error>
    {
        self.active()?;
        if layout.size() == 0
        {
            return Err(Error::Layout);
        }
        let mut next = self.free;
        let mut steps = 0usize;
        let mut largest = 0usize;
        let mut needs_metadata = false;
        while let Some(position) = next
        {
            steps = steps.checked_add(1).ok_or(Error::Counters)?;
            let maximum = self.live
                              .checked_add(self.reserved)
                              .and_then(|n| n.checked_mul(2))
                              .and_then(|n| n.checked_add(1))
                              .ok_or(Error::Counters)?;
            if steps > maximum
            {
                return Err(Error::Topology);
            }
            let node = self.arena.read(position)?;
            self.validate_free(position, node)?;
            self.validate_neighbors(position, node)?;
            let address = base.checked_add(node.offset).ok_or(Error::Layout)?;
            let Some(aligned) = address.checked_add(layout.align() - 1)
                                       .map(|address| address & !(layout.align() - 1))
            else
            {
                // A valid Layout can have no representable aligned address in
                // this mapped extent. It is a no-fit, not malformed caller data.
                next = node.free_next;
                continue;
            };
            let offset = aligned.checked_sub(base).ok_or(Error::Layout)?;
            let end = node.offset
                          .checked_add(node.bytes)
                          .filter(|end| *end <= capacity)
                          .ok_or(Error::Topology)?;
            largest = largest.max(end.saturating_sub(offset));
            if offset.checked_add(layout.size()).is_some_and(|last| last <= end)
            {
                let prefix = offset - node.offset;
                let suffix = end - offset - layout.size();
                let needed = usize::from(prefix != 0) + usize::from(suffix != 0);
                if self.arena.spares < needed
                {
                    // An exact/lower-split fit later in the same list may
                    // avoid metadata allocation entirely.
                    needs_metadata = true;
                    next = node.free_next;
                    continue;
                }
                let reserved = self.reserved.checked_add(1).ok_or(Error::Counters)?;
                let free_bytes =
                    self.free_bytes.checked_sub(layout.size()).ok_or(Error::Counters)?;
                self.unlink_free(position)?;
                let left = if prefix != 0 { Some(self.arena.take()?) } else { None };
                let right = if suffix != 0 { Some(self.arena.take()?) } else { None };
                if let Some(left) = left
                {
                    self.arena.write(left,
                                      Node::segment(node.offset,
                                                    prefix,
                                                    Kind::Free,
                                                    node.previous,
                                                    Some(position)))?;
                }
                if let Some(right) = right
                {
                    self.arena.write(right,
                                      Node::segment(offset + layout.size(),
                                                    suffix,
                                                    Kind::Free,
                                                    Some(position),
                                                    node.next))?;
                }
                self.arena.write(position,
                                  Node::segment(offset,
                                                layout.size(),
                                                Kind::Reserved(layout),
                                                left.or(node.previous),
                                                right.or(node.next)))?;
                let first = left.unwrap_or(position);
                let last = right.unwrap_or(position);
                if let Some(previous) = node.previous
                {
                    let mut other = self.arena.read(previous)?;
                    other.next = Some(first);
                    self.arena.write(previous, other)?;
                }
                else
                {
                    self.head = first;
                }
                if let Some(next) = node.next
                {
                    let mut other = self.arena.read(next)?;
                    other.previous = Some(last);
                    self.arena.write(next, other)?;
                }
                if let Some(left) = left
                {
                    self.push_free(left)?;
                }
                if let Some(right) = right
                {
                    self.push_free(right)?;
                }
                self.reserved = reserved;
                self.free_bytes = free_bytes;
                return Ok(position);
            }
            next = node.free_next;
        }
        if needs_metadata
        {
            return Err(self.arena.requirement()?);
        }
        Err(Error::NoFit { requested:         layout.size(),
                           alignment:         layout.align(),
                           free_bytes:        self.free_bytes,
                           largest_candidate: largest })
    }

    fn commit(&mut self, p: Position) -> Result<(), Error>
    {
        self.active()?;
        let mut node = self.arena.read(p)?;
        let Kind::Reserved(layout) = node.kind
        else
        {
            return Err(Error::State);
        };
        let reserved = self.reserved.checked_sub(1).ok_or(Error::Counters)?;
        let live = self.live.checked_add(1).ok_or(Error::Counters)?;
        node.kind = Kind::Live(layout);
        self.arena.write(p, node)?;
        self.reserved = reserved;
        self.live = live;
        Ok(())
    }

    fn release_plan(&self, p: Position, reservation: bool, capacity: usize)
                    -> Result<ReleasePlan, Error>
    {
        self.active()?;
        let node = self.arena.read(p)?;
        match (reservation, node.kind)
        {
            (true, Kind::Reserved(_)) | (false, Kind::Live(_)) => (),
            _ => return Err(Error::State)
        }
        self.validate_neighbors(p, node)?;
        if node.offset.checked_add(node.bytes).is_none_or(|end| end > capacity)
           || (node.next.is_none() && node.offset.checked_add(node.bytes) != Some(capacity))
        {
            return Err(Error::Topology);
        }
        for adjacent in [node.previous, node.next].into_iter().flatten()
        {
            let other = self.arena.read(adjacent)?;
            if other.kind == Kind::Free
            {
                self.validate_free(adjacent, other)?;
            }
        }
        let free_bytes = self.free_bytes
                             .checked_add(node.bytes)
                             .filter(|bytes| *bytes <= capacity)
                             .ok_or(Error::Counters)?;
        let reserved = if reservation
        {
            self.reserved.checked_sub(1).ok_or(Error::Counters)?
        }
        else
        {
            self.reserved
        };
        let live = if reservation
        {
            self.live
        }
        else
        {
            self.live.checked_sub(1).ok_or(Error::Counters)?
        };
        Ok(ReleasePlan { node,
                         free_bytes,
                         reserved,
                         live })
    }

    fn release(&mut self, p: Position, reservation: bool, capacity: usize) -> Result<bool, Error>
    {
        let ReleasePlan { mut node,
                          free_bytes,
                          reserved,
                          live } = self.release_plan(p, reservation, capacity)?;
        let mut result = p;
        node.kind = Kind::Free;
        self.arena.write(p, node)?;
        if let Some(previous) = node.previous
        {
            let mut other = self.arena.read(previous)?;
            if other.kind == Kind::Free
            {
                self.unlink_free(previous)?;
                // unlink changed the actual free-list links; never write a
                // stale snapshot back into the coalesced segment.
                other = self.arena.read(previous)?;
                other.next = node.next;
                other.bytes = other.bytes.checked_add(node.bytes).ok_or(Error::Counters)?;
                if let Some(next) = node.next
                {
                    let mut after = self.arena.read(next)?;
                    after.previous = Some(previous);
                    self.arena.write(next, after)?;
                }
                self.arena.write(previous, other)?;
                self.arena.put(p)?;
                result = previous;
                node = other;
            }
        }
        if let Some(next) = node.next
        {
            let other = self.arena.read(next)?;
            if other.kind == Kind::Free
            {
                self.unlink_free(next)?;
                node.bytes = node.bytes.checked_add(other.bytes).ok_or(Error::Counters)?;
                node.next = other.next;
                if let Some(after) = other.next
                {
                    let mut last = self.arena.read(after)?;
                    last.previous = Some(result);
                    self.arena.write(after, last)?;
                }
                self.arena.write(result, node)?;
                self.arena.put(next)?;
            }
        }
        self.push_free(result)?;
        self.free_bytes = free_bytes;
        self.reserved = reserved;
        self.live = live;
        if live == 0 && reserved == 0
        {
            let node = self.arena.read(result)?;
            if node.offset != 0
               || node.bytes != capacity
               || node.previous.is_some()
               || node.next.is_some()
               || self.head != result
               || node.free_previous.is_some()
               || node.free_next.is_some()
               || self.free != Some(result)
            {
                return Err(Error::Topology);
            }
            self.phase = Phase::Retiring;
            return Ok(true);
        }
        Ok(false)
    }
}

/// Holds real pages and their original physical-capacity charge exactly once.
/// Native HeapArc anchors every preparing/live/quarantined extent; SDK code
/// never receives this Arc or any private segment token.
pub struct Extent<'pool, 'storage, G: InvocationGate>
{
    state:    Spinlock<State>,
    base:     usize,
    capacity: usize,
    backing:  PageBlock<'pool, 'storage, G>,
    charge:   Committed<G>
}
pub struct PrepareFailure<'pool, 'storage, G: InvocationGate>
{
    pub cause:   Error,
    pub nodes:   PreparedNodes,
    pub backing: PageBlock<'pool, 'storage, G>,
    pub charge:  Committed<G>
}
pub struct InstallFailure
{
    pub cause: Error,
    pub nodes: PreparedNodes
}
#[derive(Clone, Copy, Debug)]
pub struct SpanInfo
{
    pub offset: usize,
    pub bytes:  usize,
    pub layout: Layout
}
impl<'pool, 'storage, G: InvocationGate> Extent<'pool, 'storage, G>
{
    /// Arithmetic metadata, never a mapping or pointer-dereference grant. The
    /// native consumer obtains this extent only from its real pages factory.
    pub fn base(&self) -> usize
    {
        self.base
    }

    /// Retire a genuine newly-grown/unused extent, including owner stop or
    /// metadata refusal before the first span. No duplicate empty owner is
    /// minted: only Active may perform this one-way transition.
    pub fn retire_empty(this: &Arc<Self>) -> Result<EmptyExtent<'pool, 'storage, G>, Error>
    {
        G::run(|| {
            let mut state = this.state.lock();
            state.active()?;
            if state.live != 0 || state.reserved != 0
            {
                return Err(Error::NotEmpty);
            }
            let node = match state.arena.read(state.head)
            {
                Ok(node) => node,
                Err(cause) =>
                {
                    state.phase = Phase::Quarantined;
                    return Err(cause);
                }
            };
            if state.free_bytes != this.capacity
               || node.kind != Kind::Free
               || node.offset != 0
               || node.bytes != this.capacity
               || node.previous.is_some()
               || node.next.is_some()
               || node.free_previous.is_some()
               || node.free_next.is_some()
               || state.free != Some(state.head)
            {
                state.phase = Phase::Quarantined;
                return Err(Error::Topology);
            }
            state.phase = Phase::Retiring;
            Ok(EmptyExtent { extent: this.clone() })
        })
    }

    /// The native bridge supplies visible_base from its actual pinned mapping.
    /// A number here is arithmetic only, never permission to dereference RAM.
    /// The genuine receipt must already cover the complete rounded PageBlock.
    pub fn prepare(backing: PageBlock<'pool, 'storage, G>, charge: Committed<G>,
                   visible_base: usize, mut nodes: PreparedNodes)
                   -> Result<Self, PrepareFailure<'pool, 'storage, G>>
    {
        let checked = (|| {
            let capacity = usize::try_from(backing.bytes()).map_err(|_| Error::Layout)?;
            if capacity == 0
               || visible_base.checked_add(capacity).is_none()
               || charge.class() != MemoryClass::Application
               || charge.bytes() != backing.bytes()
               || nodes.level != 0
               || nodes.nodes.len() != 1
            {
                return Err(Error::Layout);
            }
            Ok(capacity)
        })();
        let capacity = match checked
        {
            Ok(capacity) => capacity,
            Err(cause) =>
            {
                return Err(PrepareFailure { cause,
                                            nodes,
                                            backing,
                                            charge });
            }
        };
        let first = match Position::at(0, 0)
        {
            Ok(position) => position,
            Err(cause) =>
            {
                return Err(PrepareFailure { cause,
                                            nodes,
                                            backing,
                                            charge });
            }
        };
        let Some(node) = nodes.nodes.get_mut(0)
        else
        {
            return Err(PrepareFailure { cause: Error::MetadataPosition,
                                        nodes,
                                        backing,
                                        charge });
        };
        *node = Node::segment(0, capacity, Kind::Free, None, None);
        let mut arena = Arena::new();
        arena.blocks[0] = Some(nodes.nodes);
        arena.levels = 1;
        Ok(Self { state: Spinlock::new(State { arena,
                                               head: first,
                                               free: Some(first),
                                               phase: Phase::Active,
                                               free_bytes: capacity,
                                               reserved: 0,
                                               live: 0 }),
                  base: visible_base,
                  capacity,
                  backing,
                  charge })
    }

    /// Prepared metadata is returned on every ordinary supersession/refusal.
    /// Neither branch destroys the caller's allocation while holding the guard.
    pub fn install_nodes(&self, nodes: PreparedNodes) -> Result<(), InstallFailure>
    {
        G::run(|| {
            let mut state = self.state.lock();
            if let Err(cause) = state.active()
            {
                return Err(InstallFailure { cause,
                                            nodes });
            }
            if nodes.level < state.arena.levels
            {
                return Err(InstallFailure { cause: Error::MetadataSuperseded { level:
                                                                                   nodes.level,
                                                                               installed:
                                                                                   state.arena
                                                                                        .levels },
                                            nodes });
            }
            state.arena.install(nodes).map_err(|(cause, nodes)| {
                                          InstallFailure { cause,
                                                           nodes }
                                      })
        })
    }

    pub fn reserve(this: &Arc<Self>, layout: Layout)
                   -> Result<SpanReservation<'pool, 'storage, G>, Error>
    {
        let position = G::run(|| {
            let mut state = this.state.lock();
            let outcome = state.reserve(this.base, this.capacity, layout);
            if outcome.as_ref().is_err_and(|cause| structural(*cause))
            {
                state.phase = Phase::Quarantined;
            }
            outcome
        })?;
        // Caller retains its actual HeapArc anchor through the transaction.
        // Arc cloning allocates nothing and happens after the metadata borrow.
        Ok(SpanReservation { extent: Arc::clone(this),
                             position })
    }

    fn info(&self, position: Position, reservation: bool) -> Result<SpanInfo, Error>
    {
        G::run(|| {
            let state = self.state.lock();
            let node = state.arena.read(position)?;
            let layout = match (reservation, node.kind)
            {
                (true, Kind::Reserved(layout)) | (false, Kind::Live(layout)) => layout,
                _ => return Err(Error::State)
            };
            Ok(SpanInfo { offset: node.offset,
                          bytes: node.bytes,
                          layout })
        })
    }

    fn change(&self, operation: impl FnOnce(&mut State) -> Result<bool, Error>)
              -> Result<bool, Error>
    {
        G::run(|| {
            let mut state = self.state.lock();
            let result = operation(&mut state);
            if result.as_ref().is_err_and(|cause| structural(*cause))
            {
                state.phase = Phase::Quarantined;
            }
            result
        })
    }
}
fn structural(cause: Error) -> bool
{
    matches!(cause, Error::MetadataPosition | Error::Topology | Error::State | Error::Counters)
}
/// These private coordinates are only ever stored with the actual extent owner.
/// Dropping a token does NOT recycle its span or ACK an allocation resource.
/// The native staged RAII owner must explicitly abort/free and retain Failure
/// in its prepaid HeapArc/AALC quarantine anchor. This avoids panic-on-Drop,
/// invented IDs, generic callbacks and a core Arc-self retention cycle.
#[must_use = "the resident staged owner must commit or abort; keep a refused owner quarantined"]
pub struct SpanReservation<'pool, 'storage, G: InvocationGate>
{
    extent:   Arc<Extent<'pool, 'storage, G>>,
    position: Position
}
#[must_use = "the canonical allocation record must retain this owner until real disposal"]
pub struct AllocationSpan<'pool, 'storage, G: InvocationGate>
{
    extent:   Arc<Extent<'pool, 'storage, G>>,
    position: Position
}
pub struct Failure<T>
{
    pub cause: Error,
    pub owned: T
}
pub enum Released<'pool, 'storage, G: InvocationGate>
{
    Reusable,
    Empty(EmptyExtent<'pool, 'storage, G>)
}
#[must_use = "detach the actual native index and retain this candidate until all pins end"]
pub struct EmptyExtent<'pool, 'storage, G: InvocationGate>
{
    extent: Arc<Extent<'pool, 'storage, G>>
}
/// Field order is the actual refund order: metadata, physical backing, receipt.
/// Destroy this owner outside heap/index/ledger/pool guards, then ACK disposal.
pub struct RetiredExtent<'pool, 'storage, G: InvocationGate>
{
    _metadata: State,
    _backing:  PageBlock<'pool, 'storage, G>,
    _charge:   Committed<G>
}
impl<'pool, 'storage, G: InvocationGate> SpanReservation<'pool, 'storage, G>
{
    /// Arithmetic metadata of the actual retained extent; no mapping grant.
    pub fn base(&self) -> usize
    {
        self.extent.base
    }

    pub fn info(&self) -> Result<SpanInfo, Error>
    {
        self.extent.info(self.position, true)
    }

    pub fn commit(self) -> Result<AllocationSpan<'pool, 'storage, G>, Failure<Self>>
    {
        let result = self.extent.change(|state| state.commit(self.position).map(|_| false));
        match result
        {
            Ok(_) =>
            {
                let Self { extent,
                           position } = self;
                Ok(AllocationSpan { extent,
                                    position })
            }
            Err(cause) =>
            {
                Err(Failure { cause,
                              owned: self })
            }
        }
    }

    pub fn abort(self) -> Result<Released<'pool, 'storage, G>, Failure<Self>>
    {
        match self.extent.change(|state| state.release(self.position, true, self.extent.capacity))
        {
            Ok(true) => Ok(Released::Empty(EmptyExtent { extent: self.extent })),
            Ok(false) => Ok(Released::Reusable),
            Err(cause) =>
            {
                Err(Failure { cause,
                              owned: self })
            }
        }
    }
}
impl<'pool, 'storage, G: InvocationGate> AllocationSpan<'pool, 'storage, G>
{
    /// Arithmetic metadata of the actual retained extent; no mapping grant.
    pub fn base(&self) -> usize
    {
        self.extent.base
    }

    pub fn release(self) -> Result<Released<'pool, 'storage, G>, Failure<Self>>
    {
        match self.extent.change(|state| state.release(self.position, false, self.extent.capacity))
        {
            Ok(true) => Ok(Released::Empty(EmptyExtent { extent: self.extent })),
            Ok(false) => Ok(Released::Reusable),
            Err(cause) =>
            {
                Err(Failure { cause,
                              owned: self })
            }
        }
    }
}
impl<'pool, 'storage, G: InvocationGate> EmptyExtent<'pool, 'storage, G>
{
    pub fn base(&self) -> usize
    {
        self.extent.base
    }

    /// Called after native index detachment. Failure retains the same candidate;
    /// temporary inspection/operation pins do not prove physical disposal.
    pub fn try_retire(self) -> Result<RetiredExtent<'pool, 'storage, G>, Self>
    {
        match Arc::try_unwrap(self.extent)
        {
            Ok(Extent { state,
                        backing,
                        charge,
                        .. }) =>
            {
                Ok(RetiredExtent { _metadata: state.into_inner(),
                                   _backing:  backing,
                                   _charge:   charge })
            }
            Err(extent) => Err(Self { extent })
        }
    }
}
