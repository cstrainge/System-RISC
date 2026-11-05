//! Native no_std entry wrappers use the existing allocator and failure policy.
//! The same core-only executor also serves the std platform CRT.
use alloc::vec::Vec;

use mac_abi::{NO_MEMORY, Status};

#[used]
#[unsafe(export_name = "__system_risc_user_runtime_v1")]
#[unsafe(link_section = ".system_risc_user_runtime")]
static ENTRIES: [unsafe extern "C" fn() -> !; 2] = [callback_loop, thread_return];

struct Storage(Vec<u64>);
impl mac_traps::user_callbacks::Storage for Storage
{
    fn words(&mut self) -> &mut [u64]
    {
        self.0.as_mut_slice()
    }

    fn grow(&mut self, words: usize) -> Result<(), Status>
    {
        self.0.try_reserve_exact(words.saturating_sub(self.0.len())).map_err(|_| NO_MEMORY)?;
        self.0.resize(words, 0);
        Ok(())
    }
}
fn failure(status: Status, message: &'static [u8]) -> !
{
    let message = core::str::from_utf8(message).unwrap_or("invalid runtime diagnostic UTF-8");
    crate::panic::fail(format_args!("Application callback failure: {message}; status={status}"))
}
unsafe extern "C" fn callback_loop() -> !
{
    mac_traps::user_callbacks::run(&mut Storage(Vec::new()), failure)
}
unsafe extern "C" fn thread_return() -> !
{
    let client = super::client();
    if let Err(cause) = unsafe { mac_traps::native_threads::teardown_tls(client) }
    {
        crate::panic::fail(format_args!("Native child TLS teardown refused: {cause}"));
    }
    let mut call = mac_abi::native_threads::Call { operation: mac_abi::native_threads::EXIT,
                                                   ..Default::default() };
    if let Err(cause) = unsafe { client.native_thread(&mut call) }
    {
        crate::panic::fail(format_args!("Native child completion refused: {cause}"));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("unimp", options(noreturn));
    }
    // `unimp` is RISC-V's deliberate illegal instruction; AArch64's equivalent
    // is a software breakpoint, which takes the same terminal path.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}
