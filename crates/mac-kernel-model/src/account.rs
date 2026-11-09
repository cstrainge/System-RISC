//! Synchronized live budget admission with uniquely owned refund receipts.
//!
//! The resident registry keeps one canonical root per genuine owner. A DomainId
//! or copied usage snapshot is observational data, never authority to construct
//! an account or select a refund destination. Each receipt retains its original
//! account even after execution moves to a different subject or the root closes.
//! A retained AccountUse carries that same authority on a real thread; observing
//! a DomainId cannot construct one or redirect its charges.
//!
//! Every counter/metadata access uses the explicitly selected architecture gate
//! and the account lock. No lock escapes, allocates, destroys backing or invokes
//! callbacks. Atomic Arc ownership operations do not themselves need that lock;
//! the native allocator owns its own architecture boundary for deallocation.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::fmt;

use mac_object_table::{scope::{AllocationAdmission, Lease},
                       shared::{InvocationGate, OwnerRef}};
use spinning_top::Spinlock;

use crate::{DomainId,
            policy::{AccountUsage, BudgetExceeded, ConfiguredRange, DomainAccount, MemoryClass}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Closed
    {
        owner: DomainId
    },
    Budget(BudgetExceeded),
    Operation(mac_object_table::scope::Failure),
    ForeignAccount
    {
        original: DomainId,
        offered:  DomainId
    },
    Metadata
    {
        owner: DomainId
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Closed { owner } =>
            {
                write!(output, "memory account for {owner} is closed or its owner is stopping")
            }
            Self::Budget(error) => error.fmt(output),
            Self::ForeignAccount { original,
                                   offered } =>
            {
                write!(output,
                       "replacement allocation offered a different canonical account: original \
                        {original}, offered {offered}; matching labels do not grant account \
                        authority")
            }
            Self::Operation(error) =>
            {
                write!(output, "original operation allocation refused: {error}")
            }
            Self::Metadata { owner } =>
            {
                write!(output, "memory account metadata allocation refused for {owner}")
            }
        }
    }
}
impl core::error::Error for Error
{
}

#[derive(Clone, Copy)]
enum Class
{
    Kernel,
    Application
}
impl Class
{
    fn public(self) -> MemoryClass
    {
        match self
        {
            Self::Kernel => MemoryClass::Kernel,
            Self::Application => MemoryClass::Application
        }
    }
}
struct State
{
    account: DomainAccount,
    closing: bool
}
impl State
{
    fn usage(&mut self, class: Class) -> &mut AccountUsage
    {
        match class
        {
            Class::Kernel => self.account.kernel_mut(),
            Class::Application => self.account.application_mut()
        }
    }

    fn empty(&self) -> bool
    {
        self.account.kernel().reserved() == 0
        && self.account.kernel().committed() == 0
        && self.account.application().reserved() == 0
        && self.account.application().committed() == 0
    }
}
struct Inner<G: InvocationGate>
{
    owner: OwnerRef<G>,
    state: Spinlock<State>
}
/// One atomic observation of both budget classes. This is not allocation or
/// retirement authority; a closing account may still own zero-byte receipt pins.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot
{
    pub kernel:        AccountUsage,
    pub application:   AccountUsage,
    pub closing:       bool,
    /// Observation only: original receipts and admitted uses beyond the root.
    /// Admission and retirement still use the canonical account's own gate.
    pub retained_pins: usize
}
/// The unique canonical account root. Cloning a receipt's admitted Arc does not
/// mint another root or reset a budget. This type deliberately is not Clone.
/// An observed identity cannot substitute for authentic ledger authority:
/// ```compile_fail
/// use mac_kernel_model::{account::Account, policy::ConfiguredRange, DomainId};
/// use mac_object_table::shared::InvocationGate;
/// struct Gate;
/// impl InvocationGate for Gate {
///     fn run<R>(operation: impl FnOnce() -> R) -> R { operation() }
/// }
/// let limit = ConfiguredRange::new(0, 1024, 1024).unwrap();
/// let forged = Account::<Gate>::try_new(DomainId::SYSTEM, limit, limit);
/// ```
pub struct Account<G: InvocationGate>
{
    inner: Arc<Inner<G>>
}
impl<G: InvocationGate> Account<G>
{
    pub fn try_new(owner: OwnerRef<G>, kernel: ConfiguredRange, application: ConfiguredRange)
                   -> Result<Self, Error>
    {
        let identity = owner.owner();
        if !owner.accepting()
        {
            return Err(Error::Closed { owner: identity });
        }
        // This is detached authority metadata, not a published counter change.
        // Neither its allocation nor rejected Arc destruction belongs inside
        // the interrupt-masked invocation gate. Recheck the real admission bit
        // after construction; an observed identity grants no authority.
        let inner = G::with_foundation_allocation(|| {
                        Arc::try_new(Inner { state:
                                         Spinlock::new(State { account:
                                                                   DomainAccount::new(identity,
                                                                                      kernel,
                                                                                      application),
                                                               closing: false }),
                                     owner })
                    }).map_err(|_| Error::Metadata { owner: identity })?;
        let accepting = G::run(|| inner.owner.accepting());
        if !accepting
        {
            return Err(Error::Closed { owner: identity });
        }
        Ok(Self { inner })
    }

    pub fn owner(&self) -> DomainId
    {
        G::run(|| self.inner.owner.owner())
    }

    /// Compare the original account root without admitting another use or
    /// treating a matching owner identity as authority for a different root.
    pub fn owns_use(&self, usage: &AccountUse<G>) -> bool
    {
        Arc::ptr_eq(&self.inner, &usage.inner)
    }

    pub fn kernel(&self) -> AccountUsage
    {
        G::run(|| *self.inner.state.lock().account.kernel())
    }

    pub fn application(&self) -> AccountUsage
    {
        G::run(|| *self.inner.state.lock().account.application())
    }

    /// Copy both counter classes and close state under one metadata lock.
    pub fn snapshot(&self) -> Snapshot
    {
        G::run(|| {
            let state = self.inner.state.lock();
            Snapshot { kernel:        *state.account.kernel(),
                       application:   *state.account.application(),
                       closing:       state.closing,
                       retained_pins: Arc::strong_count(&self.inner) - 1 }
        })
    }

    pub fn reserve_kernel(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.reserve(Class::Kernel, bytes)
    }

    pub fn reserve_application(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.reserve(Class::Application, bytes)
    }

    fn reserve(&self, class: Class, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.inner.reserve(class, bytes)
    }

    /// Retain this exact account for an admitted execution context. The pin is
    /// created under the same closure gate as reservations; it cannot reset a
    /// budget or authorize retirement. Creation and cloning allocate nothing.
    pub fn admit_use(&self) -> Result<AccountUse<G>, Error>
    {
        G::run(|| {
            let state = self.inner.state.lock();
            let owner = self.inner.owner.owner();
            if state.closing || !self.inner.owner.accepting()
            {
                return Err(Error::Closed { owner });
            }
            Ok(AccountUse { inner:    self.inner.clone(),
                            _release: ReleaseWake { owner: self.inner.owner.clone() } })
        })
    }

    /// Close admission atomically with the usage/pin barrier. An earlier
    /// reservation may still commit (no budget growth); it must refund before
    /// retirement. A false result needs a later retry after the release wake.
    pub fn close_and_ready(&self) -> bool
    {
        G::run(|| {
            let mut state = self.inner.state.lock();
            state.closing = true;
            state.empty() && Arc::strong_count(&self.inner) == 1
        })
    }
}
impl<G: InvocationGate> Drop for Account<G>
{
    fn drop(&mut self)
    {
        G::run(|| self.inner.state.lock().closing = true);
        // The default atomic Arc field drop runs after the metadata lock/gate.
        // Outstanding receipts retain the real owner and counters until refund.
    }
}
/// Retained allocation authority, derived only from a canonical account root.
/// Clones share that original account; they neither create an account nor keep
/// admission open after its owner stops. A live use pins account retirement,
/// including when it currently owns no charged bytes.
///
/// An observed identity cannot manufacture this authority:
/// ```compile_fail
/// use mac_kernel_model::{account::AccountUse, DomainId};
/// use mac_object_table::shared::InvocationGate;
/// fn forge<G: InvocationGate>(owner: DomainId) -> AccountUse<G> {
///     AccountUse { owner }
/// }
/// ```
/// Uses cannot close or reset their original root:
/// ```compile_fail
/// use mac_kernel_model::account::AccountUse;
/// use mac_object_table::shared::InvocationGate;
/// fn close<G: InvocationGate>(authority: AccountUse<G>) {
///     authority.close_and_ready();
/// }
/// ```
pub struct AccountUse<G: InvocationGate>
{
    // Fields drop in declaration order: release the Arc before waking a reaper
    // which can immediately check the root's exact outstanding-pin barrier.
    inner:    Arc<Inner<G>>,
    _release: ReleaseWake<G>
}
impl<G: InvocationGate> AccountUse<G>
{
    /// Bind this canonical account to an already-published genuine operation.
    /// Ordinary owner stop does not revoke an admitted operation's continuation;
    /// a closed root or closed account still refuses. No budget is reset, and
    /// the capability cannot reopen ordinary owner/account admission.
    pub fn for_operation(&self, scope: &Lease<G>) -> Result<OperationAccountUse<G>, Error>
    {
        let owner = self.inner.owner.owner();
        if scope.subject() != owner
        {
            return Err(Error::Operation(mac_object_table::scope::Failure::ForeignOwner {
                expected: owner, actual: scope.subject()
            }));
        }
        let admission = scope.allocation_admission().map_err(Error::Operation)?;
        G::run(|| {
            if self.inner.state.lock().closing
            {
                return Err(Error::Closed { owner });
            }
            Ok(OperationAccountUse { account: self.clone(),
                                     admission })
        })
    }

    pub fn owner(&self) -> DomainId
    {
        G::run(|| self.inner.owner.owner())
    }

    pub fn reserve_kernel(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.inner.reserve(Class::Kernel, bytes)
    }

    pub fn reserve_application(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.inner.reserve(Class::Application, bytes)
    }
}
impl<G: InvocationGate> Clone for AccountUse<G>
{
    fn clone(&self) -> Self
    {
        Self { inner:    self.inner.clone(),
               _release: ReleaseWake { owner: self._release.owner.clone() } }
    }
}
/// Allocation authority for the continuation of one admitted resource root.
/// No caller flag or copied DomainId can construct it. The root's independent
/// close gate serializes admission without reacquiring the owner ledger during
/// allocator-backed ledger growth. This is not a physical-memory reservation.
pub struct OperationAccountUse<G: InvocationGate>
{
    account:   AccountUse<G>,
    admission: AllocationAdmission<G>
}
impl<G: InvocationGate> Clone for OperationAccountUse<G>
{
    fn clone(&self) -> Self
    {
        Self { account:   self.account.clone(),
               admission: self.admission.clone() }
    }
}
impl<G: InvocationGate> OperationAccountUse<G>
{
    pub fn owner(&self) -> DomainId
    {
        self.account.owner()
    }

    pub fn for_operation(&self, scope: &Lease<G>) -> Result<Self, Error>
    {
        self.account.for_operation(scope)
    }

    pub fn reserve_kernel(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.account.inner.reserve_operation(&self.admission, Class::Kernel, bytes)
    }

    pub fn reserve_application(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        self.account.inner.reserve_operation(&self.admission, Class::Application, bytes)
    }
}

struct ReleaseWake<G: InvocationGate>
{
    // Independent ledger authority is not an account Arc: it cannot keep the
    // just-released account pin alive while the observer checks that barrier.
    owner: OwnerRef<G>
}
impl<G: InvocationGate> Drop for ReleaseWake<G>
{
    fn drop(&mut self)
    {
        // AccountUse's Arc has already been dropped, and neither an account
        // lock nor an owner lock is held across the notification.
        G::released(&self.owner);
    }
}
impl<G: InvocationGate> Inner<G>
{
    fn reserve(self: &Arc<Self>, class: Class, bytes: u64) -> Result<Reserved<G>, Error>
    {
        G::run(|| self.reserve_authorized(class, bytes, None))
    }

    fn reserve_operation(self: &Arc<Self>, admission: &AllocationAdmission<G>, class: Class,
                         bytes: u64)
                         -> Result<Reserved<G>, Error>
    {
        // The admission guard is per root. Neither its bounded callback nor
        // counter admission touches the owner ledger or allocates backing.
        G::run(|| {
            admission.with_admission(|| self.reserve_authorized(class, bytes, Some(admission)))
        }).map_err(Error::Operation)?
    }

    fn reserve_authorized(self: &Arc<Self>, class: Class, bytes: u64,
                          admission: Option<&AllocationAdmission<G>>)
                          -> Result<Reserved<G>, Error>
    {
        let mut state = self.state.lock();
        let owner = self.owner.owner();
        if let Some(admission) = admission
        {
            if admission.subject() != owner
            {
                return Err(Error::Operation(mac_object_table::scope::Failure::ForeignOwner {
                    expected: owner, actual: admission.subject()
                }));
            }
        }
        if state.closing || (admission.is_none() && !self.owner.accepting())
        {
            return Err(Error::Closed { owner });
        }
        state.usage(class).reserve(owner, class.public(), bytes).map_err(Error::Budget)?;
        Ok(Reserved { receipt: Receipt { inner: Some(self.clone()),
                                         owner,
                                         class,
                                         bytes,
                                         phase: Phase::Reserved,
                                         admission: admission.cloned() } })
    }
}
#[derive(Clone, Copy)]
enum Phase
{
    Reserved,
    Committed
}
struct Receipt<G: InvocationGate>
{
    inner:     Option<Arc<Inner<G>>>,
    owner:     DomainId,
    class:     Class,
    bytes:     u64,
    phase:     Phase,
    admission: Option<AllocationAdmission<G>>
}
impl<G: InvocationGate> Receipt<G>
{
    fn commit(&mut self)
    {
        G::run(|| {
            // None is the terminal state used only by this private type's Drop.
            // A live public Reserved always owns exactly one admitted pin.
            if let Some(inner) = &self.inner
            {
                inner.state.lock().usage(self.class).commit_reserved(self.bytes);
                self.phase = Phase::Committed;
            }
        });
    }
}
impl<G: InvocationGate> Drop for Receipt<G>
{
    fn drop(&mut self)
    {
        let owner = G::run(|| {
            let inner = self.inner.take()?;
            {
                let mut state = inner.state.lock();
                let usage = state.usage(self.class);
                match self.phase
                {
                    Phase::Reserved => usage.release_reserved(self.bytes),
                    Phase::Committed => usage.release_committed(self.bytes)
                }
            }
            // No counter lock is held over backing/owner destruction. Release
            // this pin before waking a reaper that will inspect strong_count.
            let owner = inner.owner.clone();
            drop(inner);
            Some(owner)
        });
        if let Some(owner) = owner
        {
            G::released(&owner);
        }
    }
}
/// A unique admission which cannot be cloned or committed twice.
/// ```compile_fail
/// use mac_kernel_model::account::Reserved;
/// use mac_object_table::shared::InvocationGate;
/// fn duplicate<G: InvocationGate>(receipt: Reserved<G>) { let second = receipt.clone(); }
/// ```
/// ```compile_fail
/// use mac_kernel_model::account::Reserved;
/// use mac_object_table::shared::InvocationGate;
/// fn settle_twice<G: InvocationGate>(receipt: Reserved<G>) {
///     let first = receipt.commit();
///     let second = receipt.commit();
/// }
/// ```
#[must_use = "Drop cancels the reservation; commit transfers its existing charge"]
pub struct Reserved<G: InvocationGate>
{
    receipt: Receipt<G>
}
impl<G: InvocationGate> Reserved<G>
{
    pub fn owner(&self) -> DomainId
    {
        G::run(|| self.receipt.owner)
    }

    pub fn bytes(&self) -> u64
    {
        G::run(|| self.receipt.bytes)
    }

    pub fn class(&self) -> MemoryClass
    {
        G::run(|| self.receipt.class.public())
    }

    /// This moves already admitted bytes; closing cannot refuse settlement.
    pub fn commit(self) -> Committed<G>
    {
        let Self { mut receipt } = self;
        receipt.commit();
        Committed { receipt }
    }
}
#[must_use = "retain this receipt with its backing; Drop refunds the original account"]
pub struct Committed<G: InvocationGate>
{
    receipt: Receipt<G>
}
impl<G: InvocationGate> Committed<G>
{
    /// Reserve replacement backing against this receipt's original account and
    /// class. Closing admission still refuses growth, even if a different
    /// subject or System is currently executing the reallocating call.
    pub fn reserve_replacement(&self, bytes: u64) -> Result<Reserved<G>, Error>
    {
        let Some(inner) = &self.receipt.inner
        else
        {
            return Err(Error::Closed { owner: self.receipt.owner });
        };
        match &self.receipt.admission
        {
            Some(admission) =>
            {
                match inner.reserve_operation(admission, self.receipt.class, bytes)
                {
                    // A surviving buffer can grow after its first I/O root ends
                    // while its ORIGINAL owner/account still admits ordinary use.
                    // Budget failure and account closure never select another pool.
                    Err(Error::Operation(mac_object_table::scope::Failure::Closed)) =>
                    {
                        inner.reserve(self.receipt.class, bytes)
                    }
                    result => result
                }
            }
            None => inner.reserve(self.receipt.class, bytes)
        }
    }

    /// Offer a real continuation of the exact original account for a buffer
    /// surviving an earlier operation. The offered execution class cannot
    /// redirect this receipt's class or refund destination. This admits no new
    /// ordinary work and cannot reopen a closed account or resource root.
    pub fn reserve_replacement_in(&self, bytes: u64, continuation: &OperationAccountUse<G>)
                                  -> Result<Reserved<G>, Error>
    {
        let Some(inner) = &self.receipt.inner
        else
        {
            return Err(Error::Closed { owner: self.receipt.owner });
        };
        if !Arc::ptr_eq(inner, &continuation.account.inner)
        {
            return Err(Error::ForeignAccount { original: self.receipt.owner,
                                               offered:  continuation.owner() });
        }
        inner.reserve_operation(&continuation.admission, self.receipt.class, bytes)
    }

    pub fn owner(&self) -> DomainId
    {
        G::run(|| self.receipt.owner)
    }

    pub fn bytes(&self) -> u64
    {
        G::run(|| self.receipt.bytes)
    }

    pub fn class(&self) -> MemoryClass
    {
        G::run(|| self.receipt.class.public())
    }
}

