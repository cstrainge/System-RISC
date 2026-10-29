//! Checked worker transport. Tokens carry no Account/Context/code references;
//! the resident authenticates the actual executing module on every operation.
use core::ptr;

use mac_abi::{BAD_ABI, NOT_FOUND, NOT_SUPPORTED, OK, Status};

use crate::{Client, Error, Result,
            abi::{self, worker}};

pub const MAX_NAME_BYTES: usize = mac_module::MAX_NAME_BYTES;

/// A placement constraint over the CPUs the core can actually dispatch on.
/// This never grants CPU admission or bypasses a service's genuine affinity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum WorkerAffinity
{
    /// Follow the eligible CPU set as it grows; currently primary-only.
    #[default]
    Automatic = worker::AFFINITY_AUTOMATIC,
    /// Keep this worker on the actual primary CPU for a genuine affinity need.
    Primary   = worker::AFFINITY_PRIMARY
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerState
{
    Prepared,
    Armed,
    Running,
    Parked,
    Finished
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerStatus
{
    pub state:       WorkerState,
    pub cancelled:   bool,
    /// Actual result only when state is Finished.
    pub exit_status: Status
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerHandle
{
    token: u64
}

fn request(operation: u32, token: u64) -> abi::WorkerCall
{
    abi::WorkerCall { header: abi::Header::of::<abi::WorkerCall>(),
                      operation,
                      flags: 0,
                      worker: token,
                      specification: ptr::null(),
                      state: 0,
                      exit_status: OK,
                      microseconds: 0 }
}
fn response(call: &abi::WorkerCall) -> Result<()>
{
    if !call.header.valid::<abi::WorkerCall>()
       || call.flags & !worker::CANCEL_REQUESTED != 0
       || (!matches!(call.operation, worker::MONOTONIC | worker::PARK_FOR)
           && call.microseconds != 0)
    {
        return Err(Error(BAD_ABI));
    }
    Ok(())
}
fn invoke(host: &abi::Host, call: &mut abi::WorkerCall) -> Result<()>
{
    let callback = host.workers.ok_or(Error(NOT_SUPPORTED))?;
    // SAFETY: the validated callback borrows this initialized exact wire record
    // synchronously. CREATE's captured allocation has its separate handoff.
    let status = unsafe { callback(host.context, call) };
    if status != OK
    {
        return Err(Error(if status < 0 { status } else { BAD_ABI }));
    }
    response(call)
}

/// # Safety
/// Entry/destroy own precisely the specified exclusive resident-forwarded
/// allocation. They must consume it exactly once, be in pinned module code, and
/// never unwind. Error retains SDK ownership; success irrevocably transfers it.
pub unsafe fn prepare(client: Client, specification: &abi::WorkerSpecification)
                      -> Result<WorkerHandle>
{
    let callback = client.host.workers.ok_or(Error(NOT_SUPPORTED))?;
    let mut call = request(worker::CREATE, 0);
    call.specification = specification;
    // SAFETY: the caller owns the captured state until successful publication;
    // specification and its name are borrowed only through this call.
    let status = unsafe { callback(client.host.context, &mut call) };
    if status < OK && call.worker == 0
    {
        return Err(Error(status));
    }
    // Reporting a failed handoff after publication would let SDK destroy state
    // the resident already owns. A broken handoff is terminal, never retried.
    if status != OK || call.worker == 0 || response(&call).is_err()
    {
        super::worker_fault(format_args!("operation=worker-create status={status} \
                                          published_token={}",
                                         call.worker));
    }
    Ok(WorkerHandle { token: call.worker })
}
impl WorkerHandle
{
    fn control(self, operation: u32) -> Result<abi::WorkerCall>
    {
        let host = super::allocation::host().ok_or(Error(NOT_SUPPORTED))?;
        let mut call = request(operation, self.token);
        invoke(host, &mut call)?;
        if call.worker != self.token
        {
            return Err(Error(BAD_ABI));
        }
        Ok(call)
    }

    pub fn start(self) -> Result<()>
    {
        self.control(worker::START).map(|_| ())
    }

    pub fn wake(self) -> Result<()>
    {
        self.control(worker::WAKE).map(|_| ())
    }

    pub fn cancel(self) -> Result<()>
    {
        self.control(worker::CANCEL).map(|_| ())
    }

    /// Record cancellation and eventual disposal without waiting for a live
    /// worker. The resident retains its unique owner-ledger cleanup ticket.
    pub fn dispose(self) -> Result<()>
    {
        self.control(worker::DISPOSE).map(|_| ())
    }

    pub fn status(self) -> Result<WorkerStatus>
    {
        let call = self.control(worker::STATUS)?;
        let state = match call.state
        {
            worker::PREPARED => WorkerState::Prepared,
            worker::ARMED => WorkerState::Armed,
            worker::RUNNING => WorkerState::Running,
            worker::PARKED => WorkerState::Parked,
            worker::FINISHED => WorkerState::Finished,
            _ => return Err(Error(BAD_ABI))
        };
        if call.exit_status > OK
        {
            return Err(Error(BAD_ABI));
        }
        Ok(WorkerStatus { state,
                          cancelled: call.flags & worker::CANCEL_REQUESTED != 0,
                          exit_status: call.exit_status })
    }

    /// Called by RAII Drop. NOT_FOUND means cleanup already owns/retired this
    /// generation. Any other refusal remains resident-owned and is diagnosed.
    pub fn dispose_on_drop(self)
    {
        if let Err(Error(status)) = self.dispose()
        {
            if status != NOT_FOUND
            {
                super::diagnostic(format_args!("DRIVER_WORKER_DISPOSE_RETAINED: token={} \
                                                status={status}\n",
                                               self.token));
            }
        }
    }

    pub fn cancelled(self) -> Result<bool>
    {
        self.status().map(|status| status.cancelled)
    }

    /// Sleep only through the private authenticated native switch boundary.
    pub fn park(self) -> Result<()>
    {
        let mut call = request(worker::PARK, self.token);
        native(&mut call)?;
        if call.worker != self.token
        {
            return Err(Error(BAD_ABI));
        }
        Ok(())
    }

    pub fn monotonic_microseconds(self) -> Result<u64>
    {
        let mut call = request(worker::MONOTONIC, self.token);
        native(&mut call)?;
        if call.worker != self.token
        {
            return Err(Error(BAD_ABI));
        }
        Ok(call.microseconds)
    }

    pub fn park_for(self, duration: core::time::Duration) -> Result<()>
    {
        // A positive sub-microsecond request rounds up rather than becoming
        // an accidental zero-duration poll. Overflow has no scheduling effect.
        let microseconds = duration.as_micros() + u128::from(duration.subsec_nanos() % 1_000 != 0);
        let microseconds = u64::try_from(microseconds).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
        let mut call = request(worker::PARK_FOR, self.token);
        call.microseconds = microseconds;
        native(&mut call)?;
        if call.worker != self.token || call.microseconds != microseconds
        {
            return Err(Error(BAD_ABI));
        }
        Ok(())
    }
}
pub fn current() -> Result<WorkerHandle>
{
    let host = super::allocation::host().ok_or(Error(NOT_SUPPORTED))?;
    let mut call = request(worker::CURRENT, 0);
    invoke(host, &mut call)?;
    if call.worker == 0
    {
        return Err(Error(BAD_ABI));
    }
    Ok(WorkerHandle { token: call.worker })
}
fn native(call: &mut abi::WorkerCall) -> Result<()>
{
    #[cfg(target_arch = "riscv64")]
    {
        let status = super::riscv64::worker(call);
        if status != OK
        {
            return Err(Error(if status < OK { status } else { BAD_ABI }));
        }
        response(call)
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = call;
        Err(Error(NOT_SUPPORTED))
    }
}
