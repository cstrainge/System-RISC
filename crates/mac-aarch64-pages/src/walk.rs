//! The AArch64 binding of the shared inspection walk.
//!
//! Same walk, this architecture's descriptor and layout.
pub use mac_kernel_model::pages::walk::{ReadTables, Span, Walker};

use crate::Error as EncodingError;

/// This architecture's encoding error is the one the walk reports.
pub type Error<E> = mac_kernel_model::pages::walk::Error<E, EncodingError>;
