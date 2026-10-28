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
        G::run(|| {
            let identity = owner.owner();
            if !owner.accepting()
            {
                return Err(Error::Closed { owner: identity });
            }
            let inner = G::with_foundation_allocation(|| {
                            Arc::try_new(Inner { state:
                                         Spinlock::new(State { account:
                                                                   DomainAccount::new(identity,
                                                                                      kernel,
                                                                                      application),
                                                               closing: false }),
                                     owner })
                        }).map_err(|_| Error::Metadata { owner: identity })?;
            // The allocation itself holds no owner/counter lock. Refuse a stop
            // that raced metadata admission before handing out the root.
            if !inner.owner.accepting()
            {
                return Err(Error::Closed { owner: identity });
            }
            Ok(Self { inner })
        })
    }

    pub fn owner(&self) -> DomainId
    {
        G::run(|| self.inner.owner.owner())
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
                            _release: ReleaseWake { owner,
                                                    _gate: core::marker::PhantomData } })
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
               _release: ReleaseWake { owner: self._release.owner,
                                       _gate: core::marker::PhantomData } }
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
    owner: DomainId,
    _gate: core::marker::PhantomData<fn() -> G>
}
impl<G: InvocationGate> Drop for ReleaseWake<G>
{
    fn drop(&mut self)
    {
        // AccountUse's Arc has already been dropped, and neither an account
        // lock nor an owner lock is held across the notification.
        G::released(self.owner);
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
            drop(inner);
            Some(self.owner)
        });
        if let Some(owner) = owner
        {
            G::released(owner);
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

#[cfg(test)]
mod tests
{
    extern crate std;
    use core::cell::{Cell, RefCell};
    use std::{string::ToString,
              sync::{Arc as Shared, Barrier, Weak},
              thread};

    use mac_object_table::domain::{DomainKind, OwnerIndex};

    use super::*;
    use crate::policy::BudgetReason;

    std::thread_local! {
        static DEPTH: Cell<usize> = const { Cell::new(0) };
        static WAKES: Cell<usize> = const { Cell::new(0) };
        static WAKE_READY: Cell<bool> = const { Cell::new(false) };
        static PROBE: RefCell<Option<Weak<Account<Gate>>>> = const { RefCell::new(None) };
    }
    struct Gate;
    impl InvocationGate for Gate
    {
        fn run<R>(operation: impl FnOnce() -> R) -> R
        {
            struct Restore;
            impl Drop for Restore
            {
                fn drop(&mut self)
                {
                    DEPTH.with(|depth| depth.set(depth.get() - 1));
                }
            }
            DEPTH.with(|depth| depth.set(depth.get() + 1));
            let _restore = Restore;
            operation()
        }

        fn released(owner: DomainId)
        {
            WAKES.with(|wakes| wakes.set(wakes.get() + 1));
            PROBE.with(|probe| {
                     if let Some(account) = probe.borrow().as_ref().and_then(Weak::upgrade)
                        && account.owner() == owner
                     {
                         // The wake may acquire metadata immediately. It must see
                         // neither the refund lock nor the just-disposed Arc pin.
                         assert!(Self::run(|| account.inner.state.try_lock().is_some()));
                         WAKE_READY.with(|ready| ready.set(account.close_and_ready()));
                     }
                 });
        }
    }
    fn owner() -> OwnerRef<Gate>
    {
        OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
    }
    fn limit(bytes: u64) -> ConfiguredRange
    {
        ConfiguredRange::new(0, bytes, bytes).unwrap()
    }
    fn account(kernel: u64, application: u64) -> Account<Gate>
    {
        Account::try_new(owner(), limit(kernel), limit(application)).unwrap()
    }
    fn used(account: &Account<Gate>) -> [u64; 4]
    {
        let kernel = account.kernel();
        let application = account.application();
        [kernel.reserved(), kernel.committed(), application.reserved(), application.committed()]
    }

    #[test]
    fn snapshot_copies_both_classes_without_closing_or_authorizing_retirement()
    {
        let account = account(64, 128);
        let kernel = account.reserve_kernel(8).unwrap().commit();
        let application = account.reserve_application(16).unwrap();
        let snapshot = account.snapshot();
        assert!(!snapshot.closing);
        assert_eq!((snapshot.kernel.reserved(), snapshot.kernel.committed()), (0, 8));
        assert_eq!((snapshot.application.reserved(), snapshot.application.committed()), (16, 0));
        drop(account.reserve_kernel(0).unwrap());
        assert!(!account.close_and_ready());
        assert!(account.snapshot().closing);
        drop(kernel);
        drop(application);
        assert!(account.close_and_ready());
        let snapshot = account.snapshot();
        assert!(snapshot.closing);
        assert_eq!((snapshot.kernel.reserved(), snapshot.kernel.committed()), (0, 0));
        assert_eq!((snapshot.application.reserved(), snapshot.application.committed()), (0, 0));
    }

    #[test]
    fn reservations_commit_and_refund_the_one_counter_model()
    {
        let account = account(100, 80);
        let identity = account.owner();
        let reserved = account.reserve_kernel(60).unwrap();
        assert_eq!(reserved.owner(), identity);
        assert_eq!(reserved.class(), MemoryClass::Kernel);
        assert_eq!(reserved.bytes(), 60);
        let application = account.reserve_application(80).unwrap();
        assert_eq!(used(&account), [60, 0, 80, 0]);
        let failure = account.reserve_kernel(41).err().unwrap();
        assert_eq!(failure,
                   Error::Budget(BudgetExceeded { owner:     identity,
                                                  class:     MemoryClass::Kernel,
                                                  used:      60,
                                                  limit:     100,
                                                  requested: 41,
                                                  reason:    BudgetReason::DomainLimit }));
        assert!(failure.to_string().contains("41 bytes requested"));
        assert_eq!(used(&account), [60, 0, 80, 0]);
        let committed = reserved.commit();
        assert_eq!(committed.owner(), identity);
        assert_eq!(committed.class(), MemoryClass::Kernel);
        assert_eq!(committed.bytes(), 60);
        let remaining = account.reserve_kernel(40).unwrap();
        assert_eq!(used(&account), [40, 60, 80, 0]);
        drop(application);
        drop(committed);
        assert_eq!(used(&account), [40, 0, 0, 0]);
        drop(remaining);
        assert_eq!(used(&account), [0; 4]);
        assert!(account.close_and_ready());
    }

    #[test]
    fn limits_include_reservations_and_overflow_never_changes_counters()
    {
        let account = account(u64::MAX, 10);
        let first = account.reserve_kernel(5).unwrap().commit();
        let remaining = account.reserve_kernel(u64::MAX - 5).unwrap();
        assert_eq!(used(&account), [u64::MAX - 5, 5, 0, 0]);
        let Error::Budget(failure) = account.reserve_kernel(1).err().unwrap()
        else
        {
            panic!("missing budget detail");
        };
        assert_eq!(failure.used, u64::MAX);
        assert_eq!(failure.requested, 1);
        assert_eq!(failure.reason, BudgetReason::AccountingOverflow);
        assert!(failure.to_string().contains("AccountingOverflow"));
        let remaining = remaining.commit();
        assert_eq!(used(&account), [0, u64::MAX, 0, 0]);
        drop(first);
        drop(remaining);
        assert_eq!(used(&account), [0; 4]);
        let range = ConfiguredRange::new(0, 7, 100).unwrap();
        let selected = Account::try_new(owner(), range, range).unwrap();
        let Error::Budget(failure) = selected.reserve_application(8).err().unwrap()
        else
        {
            panic!("missing selected limit");
        };
        assert_eq!(failure.limit, 7, "configured maximum is not the selected active budget");
        assert_eq!(failure.reason, BudgetReason::DomainLimit);
        assert_eq!(used(&selected), [0; 4]);
    }

    #[test]
    fn close_preserves_admitted_settlement_and_rejects_even_zero_byte_admission()
    {
        let account = account(64, 64);
        let reserved = account.reserve_kernel(32).unwrap();
        assert!(!account.close_and_ready());
        assert_eq!(account.reserve_application(0).err(),
                   Some(Error::Closed { owner: account.owner() }));
        let committed = reserved.commit();
        assert_eq!(used(&account), [0, 32, 0, 0]);
        assert!(!account.close_and_ready());
        drop(committed);
        assert!(account.close_and_ready());
        assert_eq!(account.reserve_kernel(1).err(), Some(Error::Closed { owner: account.owner() }));
    }

    #[test]
    fn zero_usage_pin_is_released_before_the_retirement_wake()
    {
        let account = Shared::new(account(0, 0));
        WAKES.with(|wakes| wakes.set(0));
        WAKE_READY.with(|ready| ready.set(false));
        PROBE.with(|probe| *probe.borrow_mut() = Some(Shared::downgrade(&account)));
        let pin = account.reserve_kernel(0).unwrap();
        assert_eq!(used(&account), [0; 4]);
        assert!(!account.close_and_ready(), "zero-byte receipt still pins real account lifetime");
        let pin = pin.commit();
        assert!(!account.close_and_ready());
        drop(pin);
        assert!(WAKE_READY.with(Cell::get), "wake raced the final receipt Arc release");
        assert_eq!(WAKES.with(Cell::get), 1);
        assert_eq!(DEPTH.with(Cell::get), 0);
        PROBE.with(|probe| *probe.borrow_mut() = None);
    }

    #[test]
    fn owner_stop_refuses_admission_and_dropping_root_does_not_break_receipts()
    {
        let owner = owner();
        let account = Account::try_new(owner.clone(), limit(64), limit(64)).unwrap();
        let receipt = account.reserve_kernel(16).unwrap();
        let retained = Arc::downgrade(&account.inner);
        owner.stop_admission();
        assert_eq!(account.reserve_kernel(0).err(), Some(Error::Closed { owner: owner.owner() }));
        assert_eq!(Account::try_new(owner.clone(), limit(64), limit(64)).err(),
                   Some(Error::Closed { owner: owner.owner() }));
        drop(account);
        let receipt = receipt.commit();
        assert_eq!(receipt.owner(), owner.owner());
        assert!(retained.upgrade().is_some());
        drop(receipt);
        assert!(retained.upgrade().is_none(), "final receipt must release orphaned backing");
    }

    #[test]
    fn cross_thread_drop_refunds_original_account_despite_another_active_owner()
    {
        let original = account(64, 64);
        let current = Shared::new(account(128, 128));
        let receipt = original.reserve_kernel(64).unwrap().commit();
        let other = current.reserve_kernel(32).unwrap().commit();
        let selected = current.clone();
        thread::spawn(move || {
            let current_owner = selected.owner();
            assert_ne!(receipt.owner(), current_owner);
            drop(receipt);
            assert_eq!(selected.kernel().committed(), 32);
        }).join()
          .unwrap();
        assert_eq!(used(&original), [0; 4]);
        assert_eq!(used(&current), [0, 32, 0, 0]);
        drop(other);
        assert_eq!(used(&current), [0; 4]);
    }

    #[test]
    fn retained_authorities_share_limits_and_refund_the_original_account()
    {
        let original = account(64, 32);
        let other = account(128, 128);
        let authority = original.admit_use().unwrap();
        let peer = authority.clone();
        assert_eq!(authority.owner(), original.owner());
        assert_ne!(authority.owner(), other.owner());
        let kernel = authority.reserve_kernel(64).unwrap().commit();
        let application = peer.reserve_application(32).unwrap();
        assert_eq!(used(&original), [0, 64, 32, 0]);
        assert_eq!(used(&other), [0; 4]);
        let Error::Budget(error) = peer.reserve_kernel(1).err().unwrap()
        else
        {
            panic!("retained authority must enforce its original root's limit");
        };
        assert_eq!(error.owner, original.owner());
        assert_eq!((error.used, error.limit, error.requested), (64, 64, 1));
        assert!(error.to_string().contains("1 bytes requested"));
        drop(authority);
        drop(peer);
        assert!(!original.close_and_ready());
        drop(kernel);
        drop(application);
        assert_eq!(used(&original), [0; 4]);
        assert_eq!(used(&other), [0; 4]);
        assert!(original.close_and_ready());
    }

    #[test]
    fn authority_pins_release_before_the_last_retirement_wake()
    {
        let account = Shared::new(account(0, 0));
        let first = account.admit_use().unwrap();
        let last = first.clone();
        WAKES.with(|wakes| wakes.set(0));
        WAKE_READY.with(|ready| ready.set(false));
        PROBE.with(|probe| *probe.borrow_mut() = Some(Shared::downgrade(&account)));
        assert_eq!(used(&account), [0; 4]);
        assert!(!account.close_and_ready(), "a quiescent thread's authority still pins retirement");
        drop(first);
        assert!(!WAKE_READY.with(Cell::get), "one actual thread authority remains");
        assert_eq!(WAKES.with(Cell::get), 1);
        drop(last);
        assert!(WAKE_READY.with(Cell::get), "wake must observe the disposed authority Arc");
        assert_eq!(WAKES.with(Cell::get), 2);
        assert_eq!(DEPTH.with(Cell::get), 0);
        PROBE.with(|probe| *probe.borrow_mut() = None);
    }

    #[test]
    fn stopped_owner_and_dropped_root_never_reauthorize_a_retained_use()
    {
        let owner = owner();
        let account = Account::try_new(owner.clone(), limit(64), limit(64)).unwrap();
        let authority = account.admit_use().unwrap();
        let pending = authority.reserve_kernel(16).unwrap();
        let retained = Arc::downgrade(&account.inner);
        owner.stop_admission();
        assert_eq!(account.admit_use().err(), Some(Error::Closed { owner: owner.owner() }));
        assert_eq!(authority.reserve_kernel(0).err(), Some(Error::Closed { owner: owner.owner() }));
        assert_eq!(authority.reserve_application(1).err(),
                   Some(Error::Closed { owner: owner.owner() }));
        drop(account);
        let clone = authority.clone();
        assert_eq!(clone.reserve_kernel(1).err(), Some(Error::Closed { owner: owner.owner() }));
        let committed = pending.commit();
        assert_eq!(committed.owner(), owner.owner());
        drop(committed);
        drop(authority);
        assert!(retained.upgrade().is_some(), "the last actual use retains the original account");
        drop(clone);
        assert!(retained.upgrade().is_none(), "dropping the root cannot orphan retained metadata");
        assert!(Error::Closed { owner: owner.owner() }.to_string()
                                                      .contains("closed or its owner is stopping"));
    }

    #[test]
    fn concurrent_authority_admission_and_closure_share_one_retirement_barrier()
    {
        for _ in 0..32
        {
            let account = Shared::new(account(0, 0));
            let start = Shared::new(Barrier::new(3));
            let admit = {
                let (account, start) = (account.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    account.admit_use()
                })
            };
            let close = {
                let (account, start) = (account.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    account.close_and_ready()
                })
            };
            start.wait();
            let authority = admit.join().unwrap();
            let ready = close.join().unwrap();
            match authority
            {
                Ok(authority) =>
                {
                    assert!(!ready, "retirement missed an admitted authority with zero usage");
                    assert_eq!(authority.reserve_kernel(0).err(),
                               Some(Error::Closed { owner: account.owner() }));
                    drop(authority);
                }
                Err(error) =>
                {
                    assert_eq!(error, Error::Closed { owner: account.owner() });
                    assert!(ready);
                }
            }
            assert!(account.close_and_ready());
        }
    }

    #[test]
    fn concurrent_close_and_reserve_have_one_atomic_admission_result()
    {
        for _ in 0..32
        {
            let account = Shared::new(account(1, 0));
            let start = Shared::new(Barrier::new(3));
            let allocate = {
                let (account, start) = (account.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    account.reserve_kernel(1)
                })
            };
            let close = {
                let (account, start) = (account.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    account.close_and_ready()
                })
            };
            start.wait();
            let reserved = allocate.join().unwrap();
            let ready = close.join().unwrap();
            match reserved
            {
                Ok(receipt) =>
                {
                    assert!(!ready, "retirement missed admitted bytes/pin");
                    drop(receipt);
                }
                Err(error) =>
                {
                    assert_eq!(error, Error::Closed { owner: account.owner() });
                    assert!(ready);
                }
            }
            assert!(account.close_and_ready());
        }
    }

    #[test]
    fn concurrent_receipts_share_limits_across_both_account_classes()
    {
        let account = Shared::new(account(64, 32));
        let barrier = Shared::new(Barrier::new(9));
        let mut workers = std::vec::Vec::new();
        for _ in 0..8
        {
            let (account, barrier) = (account.clone(), barrier.clone());
            workers.push(thread::spawn(move || {
                             let kernel = account.reserve_kernel(8).unwrap().commit();
                             let application = account.reserve_application(4).unwrap();
                             barrier.wait();
                             barrier.wait();
                             drop(kernel);
                             drop(application);
                         }));
        }
        barrier.wait();
        assert_eq!(used(&account), [0, 64, 32, 0]);
        for failed in [account.reserve_kernel(1).err().unwrap(),
                       account.reserve_application(1).err().unwrap()]
        {
            assert!(matches!(failed,
                             Error::Budget(BudgetExceeded { reason: BudgetReason::DomainLimit,
                                                            .. })));
        }
        barrier.wait();
        for worker in workers
        {
            worker.join().unwrap();
        }
        assert_eq!(used(&account), [0; 4]);
        assert!(account.close_and_ready());
    }
}
