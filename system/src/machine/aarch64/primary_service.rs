//! The private System-to-System gate for this architecture.
//!
//! The trap number travels in x8 and the call is an `svc`. It is materialised
//! with movz/movk because the number is too wide for a single immediate. Shared
//! code calls the symbol without naming an architecture.

use super::super::scheduler::service::host::TRAP;

core::arch::global_asm!(
    ".pushsection .text.system_primary_service,\"ax\",%progbits",
    ".balign 4",
    ".global __system_primary_service_acquire",
    ".type __system_primary_service_acquire,%function",
    "__system_primary_service_acquire:",
    "movz x8, #{low}",
    "movk x8, #{high}, lsl #16",
    ".global __system_primary_service_acquire_ecall",
    "__system_primary_service_acquire_ecall:",
    "svc #0",
    "ret",
    ".size __system_primary_service_acquire, .-__system_primary_service_acquire",
    ".popsection",
    low = const TRAP & 0xffff,
    high = const (TRAP >> 16) & 0xffff
);
