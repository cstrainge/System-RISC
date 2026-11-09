//! The RISC-V binding of the shared inspection walk.
//!
//! The walk itself lives in `mac_kernel_model::pages::walk` because it is the
//! same work on any format; this module binds it to this architecture's
//! descriptor and layout so callers keep naming `Walker` and `Error<E>`.
pub use mac_kernel_model::pages::walk::{ReadTables, Span, Walker};

use crate::Error as EncodingError;

/// This architecture's encoding error is the one the walk reports.
pub type Error<E> = mac_kernel_model::pages::walk::Error<E, EncodingError>;

#[allow(unused_imports)]
use crate::{Decoded, Entry, LeafSize, Sv39, mapping::{Mapper, Tables}};
#[allow(unused_imports)]
use mac_kernel_model::pages::{PageOwnership, Permissions, PhysicalAddress};

