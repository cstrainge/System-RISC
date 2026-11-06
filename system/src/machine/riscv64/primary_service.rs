//! The private System-to-System gate for this architecture.
//!
//! The trap number travels in a7 and the call is an `ecall`. Shared code calls
//! the symbol without naming an architecture.

use super::super::scheduler::service::host::TRAP;

core::arch::global_asm!(
    ".pushsection .text.system_primary_service,\"ax\",@progbits",
    ".balign 4",
    ".global __system_primary_service_acquire",
    ".type __system_primary_service_acquire,@function",
    "__system_primary_service_acquire:",
    "li a7, {trap}",
    ".global __system_primary_service_acquire_ecall",
    "__system_primary_service_acquire_ecall:",
    "ecall",
    "ret",
    ".size __system_primary_service_acquire, .-__system_primary_service_acquire",
    ".popsection",
    trap = const TRAP
);
