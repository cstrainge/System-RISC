//! Actual original claim return. Metadata owns no author/source/Context.
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::*;
use crate::machine::scheduler::callbacks::{ControlWaitCaller, ControlWaitTarget};

/// One admitted manager-local hint. It never grants mutation/callback authority.
pub(super) struct Presence
{
    count:   AtomicUsize,
    corrupt: AtomicBool
}
impl Presence
{
    pub(super) fn prepare() -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { count:   AtomicUsize::new(0),
                            corrupt: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)
    }

    pub(super) fn active(&self) -> Result<bool, Status>
    {
        if self.corrupt.load(Ordering::Acquire)
        {
            return Err(CORRUPT_DATA);
        }
        Ok(self.count.load(Ordering::Acquire) != 0)
    }

    fn enter(&self) -> Result<(), Status>
    {
        if self.corrupt.load(Ordering::Acquire)
        {
            return Err(CORRUPT_DATA);
        }
        self.count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| count.checked_add(1))
            .map(|_| ())
            .map_err(|_| CORRUPT_DATA)
    }

    fn leave(&self)
    {
        if self.count
               .try_update(Ordering::AcqRel, Ordering::Acquire, |count| count.checked_sub(1))
               .is_err()
        {
            // Sticky bounded observation. Availability is not an ACK; the next
            // admission returns typed corruption instead of trusting this hint.
            self.corrupt.store(true, Ordering::Release);
        }
    }
}

pub(crate) struct ClaimReturn
{
    presence: Arc<Presence>,
    state:    Spinlock<ReturnState>
}
struct ReturnState
{
    bound:    bool,
    returned: bool,
    first:    Option<Arc<WaitNode>>
}
impl ClaimReturn
{
    pub(super) fn prepare(presence: Arc<Presence>) -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { presence,
                            state: Spinlock::new(ReturnState { bound:    false,
                                                               returned: false,
                                                               first:    None }) }).map_err(|_| {
                                                                                       NO_MEMORY
                                                                                   })
    }

    pub(super) fn bind(&self) -> Result<(), Status>
    {
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut state = self.state.lock();
            if state.bound || state.returned || state.first.is_some()
            {
                return Err(BAD_ABI);
            }
            self.presence.enter()?;
            state.bound = true;
            Ok(())
        })
    }

    pub(super) fn active(&self) -> bool
    {
        crate::toolbox::handles::InvocationGate::run(|| {
            let state = self.state.lock();
            state.bound && !state.returned
        })
    }

    pub(super) fn returned(&self) -> bool
    {
        let (bound, mut next) = crate::toolbox::handles::InvocationGate::run(|| {
            let mut state = self.state.lock();
            if !state.bound || state.returned
            {
                return (false, None);
            }
            state.returned = true;
            self.presence.leave();
            (true, state.first.take())
        });
        if !bound
        {
            return false;
        }
        while let Some(node) = next
        {
            let (following, epoch) = crate::toolbox::handles::InvocationGate::run(|| {
                let mut state = node.state.lock();
                let next = state.next.take();
                let epoch = state.linked.take();
                if let Some(epoch) = epoch
                {
                    node.ready.store(epoch, Ordering::Release);
                }
                (next, epoch)
            });
            next = following;
            if let Some(epoch) = epoch
            {
                crate::machine::scheduler::callbacks::wake_control_claim(&node, epoch);
            }
            drop(node);
        }
        true
    }

    fn register(&self, node: &Arc<WaitNode>) -> Result<u64, Status>
    {
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut record = self.state.lock();
            let mut state = node.state.lock();
            if !record.bound || state.linked.is_some() || state.next.is_some()
            {
                return Err(BAD_ABI);
            }
            let epoch = node.epoch.load(Ordering::Relaxed).checked_add(1).ok_or(BAD_ABI)?;
            node.ready.store(0, Ordering::Relaxed);
            node.epoch.store(epoch, Ordering::Release);
            node.active.store(true, Ordering::Release);
            if record.returned
            {
                node.ready.store(epoch, Ordering::Release);
            }
            else
            {
                state.next = record.first.take();
                state.linked = Some(epoch);
                record.first = Some(node.clone());
            }
            Ok(epoch)
        })
    }
}
struct NodeState
{
    linked: Option<u64>,
    next:   Option<Arc<WaitNode>>
}
pub(crate) struct WaitNode
{
    target: ControlWaitTarget,
    epoch:  AtomicU64,
    ready:  AtomicU64,
    active: AtomicBool,
    state:  Spinlock<NodeState>
}
impl WaitNode
{
    pub(crate) fn target(&self) -> &ControlWaitTarget
    {
        &self.target
    }

    pub(crate) fn active_for(&self, epoch: u64) -> bool
    {
        self.active.load(Ordering::Acquire) && self.epoch.load(Ordering::Acquire) == epoch
    }

    pub(crate) fn ready_for(&self, epoch: u64) -> bool
    {
        self.active.load(Ordering::Acquire)
        && self.epoch.load(Ordering::Acquire) == epoch
        && self.ready.load(Ordering::Acquire) == epoch
    }

    fn prepare(caller: &ControlWaitCaller) -> Result<Arc<Self>, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: caller.allocation().clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::context::without_recovery(|| {
            crate::allocator::with_target(target, || {
                Arc::try_new(Self {
                target:caller.target(),epoch:AtomicU64::new(0),ready:AtomicU64::new(0),
                active:AtomicBool::new(false),state:Spinlock::new(NodeState {linked:None,next:None})
            }).map_err(|_|NO_MEMORY)
            })
        })
    }
}
#[derive(Clone)]
pub(crate) struct ClaimWaitTicket
{
    node:  Arc<WaitNode>,
    epoch: u64
}
impl ClaimWaitTicket
{
    pub(crate) fn node(&self) -> &WaitNode
    {
        &self.node
    }

    pub(crate) fn epoch(&self) -> u64
    {
        self.epoch
    }

    pub(crate) fn ready(&self) -> bool
    {
        self.node.ready_for(self.epoch)
    }
}
pub(crate) struct ClaimAdmission
{
    key:              mac_object_table::store::CompletionKey,
    recipient:        crate::context::AppId,
    window:           mac_abi::WindowId,
    tree:             tree::Node,
    source:           crate::toolbox::resources::definitions::QuiescencePlan,
    spec:             Spec,
    title:            Option<Arc<super::super::snapshot::Title>>,
    tracking:         Option<Tracking>,
    splitter:         Option<splitter::Capture>,
    scroll_axis:      Option<usize>,
    scroll_highlight: i16,
    layout:           Option<Layout>,
    inherited_clip:   Option<Rect>,
    record:           Arc<ClaimReturn>,
    caller:           Option<ControlWaitCaller>,
    node:             Option<Arc<WaitNode>>,
    ticket:           Option<ClaimWaitTicket>
}
impl Drop for ClaimAdmission
{
    fn drop(&mut self)
    {
        if let Some(node) = &self.node
        {
            node.active.store(false, Ordering::Release);
        }
    }
}
impl ClaimAdmission
{
    pub(crate) fn arm(&mut self) -> Result<(), Status>
    {
        if self.caller.is_none()
        {
            let caller = crate::machine::scheduler::callbacks::control_wait_caller()?;
            if caller.owner() != self.key.owner() || crate::context::recovery_activity().is_some()
            {
                return Err(ACCESS_DENIED);
            }
            self.caller = Some(caller);
        }
        let caller = self.caller.as_ref().ok_or(BAD_ABI)?;
        if self.node.is_none()
        {
            self.node = Some(WaitNode::prepare(caller)?);
        }
        let node = self.node.as_ref().ok_or(BAD_ABI)?;
        let epoch = self.record.register(node)?;
        self.ticket = Some(ClaimWaitTicket { node: node.clone(),
                                             epoch });
        Ok(())
    }

    pub(crate) fn ticket(&self) -> Result<ClaimWaitTicket, Status>
    {
        self.ticket.clone().ok_or(BAD_ABI)
    }

    pub(crate) fn returned(&self) -> bool
    {
        self.ticket.as_ref().is_some_and(ClaimWaitTicket::ready)
    }

    pub(crate) fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        let object = manager.snapshot(self.key.id(), self.recipient.0)?;
        if manager.objects.completion_key(self.key.id()) != Some(self.key)
           || object.window != self.window.0
           || object.tree != self.tree
           || !self.source.matches(&object.definition)
           || !match (&self.layout, &object.state.layout)
           {
               (Some(a), Some(b)) => a.same_admission_snapshot(b),
               (None, None) => true,
               _ => false
           }
           || !match (&self.title, &object.state.title)
           {
               (Some(a), Some(b)) => Arc::ptr_eq(a, b),
               (None, None) => true,
               _ => false
           }
           || self.spec != object.state.spec
           || self.tracking != object.state.tracking
           || self.scroll_axis != object.state.scroll_axis
           || self.scroll_highlight != object.state.scroll_highlight
           || self.inherited_clip != manager.inherited_clip(self.key.id())
           || !match (&self.splitter, &object.state.splitter)
           {
               (Some(a), Some(b)) => a.same_snapshot(b),
               (None, None) => true,
               _ => false
           }
        {
            return Err(OPERATION_CANCELLED);
        }
        Ok(())
    }

    pub(crate) fn replace_record(&mut self, record: Arc<ClaimReturn>) -> Result<(), Status>
    {
        self.record = record;
        self.arm()
    }
}
impl Manager
{
    fn active_claim_on_path(&self, mut id: u64) -> Result<Option<Arc<ClaimReturn>>, Status>
    {
        while id != 0
        {
            let object = self.objects.get(id).ok_or(NOT_FOUND)?;
            if object.disposal.as_ref().is_some_and(|cell| cell.active())
            {
                return Err(SERVICE_BUSY);
            }
            if let Some(link) = &object.claim
            {
                if let Some(record) = link.return_record()
                {
                    return Ok(Some(record));
                }
            }
            id = object.tree.parent;
        }
        Ok(None)
    }

    pub(crate) fn claim_blocker(&self, call: &ControlCall, owner: u64, viewport: Rect)
                                -> Result<Option<Arc<ClaimReturn>>, Status>
    {
        if !self.claim_presence.active()?
        {
            return Ok(None);
        }

        if call.operation < CREATE_STANDARD
           || matches!(call.operation,
                       CREATE_STANDARD | CREATE_SCROLLBAR | INFO_OBJECT | NEXT_CHILD_OBJECT)
        {
            return Ok(None);
        }
        let object = self.snapshot(call.id, owner)?;
        let mut record = self.active_claim_on_path(call.id)?;
        // Exact existing owned-layout footprint. These are classification only;
        // actual mutations still pass original claim gates after return.
        if record.is_some()
        {
            return Ok(record);
        }
        let layout = matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
                     && self.tracking_layout_operation(call, owner)?
                     || self.user_layout_operation(call, owner, viewport)?;
        if record.is_none() && layout && object.window != 0
        {
            for (id, item) in self.objects.iter().filter(|(_, item)| item.window == object.window)
            {
                if item.recipient != owner
                {
                    return Err(ACCESS_DENIED);
                }
                if let Some(found) = self.active_claim_on_path(id)?
                {
                    record = Some(found);
                    break;
                }
            }
        }
        else if record.is_none() && (layout || object.definition.is_process_source())
        {
            let mut id = object.tree.first;
            while id != 0
            {
                if let Some(found) = self.active_claim_on_path(id)?
                {
                    record = Some(found);
                    break;
                }
                id = self.next_in(call.id, id);
            }
        }
        Ok(record)
    }

    pub(crate) fn claim_admission(&self, call: &ControlCall, owner: u64, viewport: Rect)
                                  -> Result<Option<ClaimAdmission>, Status>
    {
        let Some(record) = self.claim_blocker(call, owner, viewport)?
        else
        {
            return Ok(None);
        };
        let object = self.snapshot(call.id, owner)?;

        Ok(Some(ClaimAdmission { key: self.objects.completion_key(call.id).ok_or(NOT_FOUND)?,
                                 recipient: crate::context::AppId(owner),
                                 window: mac_abi::WindowId(object.window),
                                 tree: object.tree,
                                 source: object.definition.quiescence_plan(),
                                 spec: object.state.spec,
                                 title: object.state.title.clone(),
                                 tracking: object.state.tracking,
                                 splitter: object.state.splitter.clone(),
                                 scroll_axis: object.state.scroll_axis,
                                 scroll_highlight: object.state.scroll_highlight,
                                 layout: object.state.layout.clone(),
                                 inherited_clip: self.inherited_clip(call.id),
                                 record,
                                 caller: None,
                                 node: None,
                                 ticket: None }))
    }
}
