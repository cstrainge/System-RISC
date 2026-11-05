//! AArch64 machine adapters.
//!
//! Only genuinely architecture-specific code belongs in this module. Logic that
//! is driven by the platform record's generated constants — physical placement,
//! DMA identity, device inventory, the scheduler's bookkeeping — is not
//! architecture-specific and must not be duplicated here; it belongs in shared
//! code above the selection point.

pub(crate) mod interrupts;
