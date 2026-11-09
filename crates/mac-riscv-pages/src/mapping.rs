//! The RISC-V binding of the shared mapping walk.
//!
//! The walk itself lives in `mac_kernel_model::pages::mapping` because it is the
//! same work on any format; this module binds it to this architecture's
//! descriptor and layout so callers keep naming `Mapper<Frames>` and
//! `MapError<FrameError>`.
pub use mac_kernel_model::pages::mapping::{Mapper, Tables};

// The tests reach these through `use super::*`, mirroring what this module
// needed when the walk lived here.
#[allow(unused_imports)]
use core::fmt;
#[allow(unused_imports)]
use mac_kernel_model::{pages::{MappingPlan, PageLayout, PageOwnership, Permissions,
                               PhysicalAddress, VirtualAddress},
                       policy::{AddressRange, ModelError}};
#[allow(unused_imports)]
use crate::{Decoded, Entry, Error, LeafSize, Sv39};

/// This architecture's encoding error is the one the walk reports.
pub type MapError<E> = mac_kernel_model::pages::mapping::MapError<E, Error>;

