//! Genuine buddy/Application-account binding. This factory derives its CPU
//! address only from the actual retained DirectMap-backed native Backing.
#![forbid(unsafe_code)]
use core::fmt;

use mac_kernel_model::{account::{Committed, Reserved},
                       heap,
                       policy::MemoryClass};
use mac_object_table::domain::{DomainId, DomainKind};

use super::{Backing, NativeInvocationGate};
use crate::toolbox::handles::OwnerRef;

type Extent = heap::Extent<'static, 'static, NativeInvocationGate>;

#[derive(Debug)]
pub(crate) enum ApplicationExtentError
{
    Pages(super::Error),
    Foreign
    {
        expected: DomainId,
        actual:   DomainId
    },
    Domain(DomainKind),
    Class(MemoryClass),
    Reservation
    {
        planned:  u64,
        reserved: u64
    },
    Backing
    {
        planned: u64,
        actual:  u64
    },
    Core(heap::Error)
}
impl ApplicationExtentError
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Pages(cause) => cause.status(),
            Self::Foreign { .. } | Self::Domain(_) | Self::Class(_) => mac_abi::ACCESS_DENIED,
            Self::Reservation { .. } | Self::Backing { .. } | Self::Core(_) => mac_abi::BAD_ABI
        }
    }
}
impl fmt::Display for ApplicationExtentError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pages(cause) => cause.fmt(out),
            Self::Foreign { expected,
                            actual } =>
            {
                write!(out,
                       "application backing receipt belongs to {actual}, expected actual heap \
                        owner {expected}")
            }
            Self::Domain(kind) =>
            {
                write!(out, "application heap requires a genuine Process owner, received {kind:?}")
            }
            Self::Class(class) => write!(out, "application backing receipt has class {class:?}"),
            Self::Reservation { planned,
                                reserved } =>
            {
                write!(out,
                       "application extent reservation differs from exact buddy plan: \
                        {reserved}/{planned} bytes")
            }
            Self::Backing { planned,
                            actual } =>
            {
                write!(out,
                       "application extent buddy backing differs from admission: \
                        {actual}/{planned} bytes")
            }
            Self::Core(cause) => cause.fmt(out)
        }
    }
}

// Variant fields have deliberate destruction order: metadata, actual pages,
// then the original reservation/charge. No cleanup ticket exists before the
// caller anchors/publishes the resulting extent in its canonical heap root.
enum HeldPreparation
{
    Reserved
    {
        _nodes: heap::PreparedNodes, _reservation: Reserved<NativeInvocationGate>
    },
    Backed
    {
        _nodes:       heap::PreparedNodes,
        _backing:     Backing,
        _reservation: Reserved<NativeInvocationGate>
    },
    Charged
    {
        _nodes:   heap::PreparedNodes,
        _backing: Backing,
        _charge:  Committed<NativeInvocationGate>
    }
}
/// Actual owned failure, not a copied accounting snapshot. Dispose outside
/// every heap/index/account/owner guard after recording its bounded cause.
#[must_use]
pub(crate) struct ApplicationExtentFailure
{
    pub(crate) cause: ApplicationExtentError,
    held:             HeldPreparation
}
impl ApplicationExtentFailure
{
    pub(crate) fn dispose(self) -> ApplicationExtentError
    {
        drop(self.held);
        self.cause
    }
}

/// The caller obtained this reservation from the actual canonical Process
/// Account before page acquisition. Matching labels below CHECK that binding;
/// they never construct an account, retarget a receipt or grant a process.
/// Growth policy and Kernel metadata preparation belong to that same real heap
/// operation outside its mutation guards. This factory adds no System-heap
/// occupancy receipt to physical application payload pages.
pub(crate) fn prepare_application_extent(owner: &OwnerRef,
                                         reservation: Reserved<NativeInvocationGate>,
                                         requested_backing: usize, nodes: heap::PreparedNodes)
                                         -> Result<Extent, ApplicationExtentFailure>
{
    let checked = (|| {
        if owner.owner().kind() != DomainKind::Process
        {
            return Err(ApplicationExtentError::Domain(owner.owner().kind()));
        }
        if reservation.owner() != owner.owner()
        {
            return Err(ApplicationExtentError::Foreign { expected: owner.owner(),
                                                         actual:   reservation.owner() });
        }
        if reservation.class() != MemoryClass::Application
        {
            return Err(ApplicationExtentError::Class(reservation.class()));
        }
        let planned =
            Backing::planned_bytes(requested_backing).map_err(ApplicationExtentError::Pages)?;
        if reservation.bytes() != planned
        {
            return Err(ApplicationExtentError::Reservation { planned,
                                                             reserved: reservation.bytes() });
        }
        Ok(planned)
    })();
    let planned = match checked
    {
        Ok(planned) => planned,
        Err(cause) =>
        {
            return Err(ApplicationExtentFailure { cause,
                                                  held: HeldPreparation::Reserved { _nodes:
                                                                                        nodes,
                                                                                    _reservation:
                                                                                        reservation } });
        }
    };
    let backing = match Backing::reserve_uninitialized(requested_backing)
    {
        Ok(backing) => backing,
        Err(cause) =>
        {
            return Err(ApplicationExtentFailure { cause: ApplicationExtentError::Pages(cause),
                                                  held:  HeldPreparation::Reserved { _nodes:
                                                                                         nodes,
                                                                                     _reservation:
                                                                                         reservation } });
        }
    };
    if backing.bytes() != planned
    {
        return Err(ApplicationExtentFailure {
            cause:ApplicationExtentError::Backing{planned,actual:backing.bytes()},
            held:HeldPreparation::Backed{_nodes:nodes,_backing:backing,_reservation:reservation}});
    }
    // Decomposition is private to the native owner; no raw Block extractor or
    // caller-selected virtual base is exposed. This exact Backing came from the
    // pinned real DirectMap and still uniquely owns its full buddy extent.
    let Backing { pages,
                  cpu_base } = backing;
    let charge = reservation.commit();
    match heap::Extent::prepare(pages,charge,cpu_base,nodes)
    {
        Ok(extent)=>Ok(extent),
        Err(failure)=>Err(ApplicationExtentFailure {
            cause:ApplicationExtentError::Core(failure.cause),
            held:HeldPreparation::Charged{_nodes:failure.nodes,
                _backing:Backing{pages:failure.backing,cpu_base},_charge:failure.charge}})
    }
}
