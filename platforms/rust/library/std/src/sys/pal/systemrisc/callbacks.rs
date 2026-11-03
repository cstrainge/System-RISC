//! Std owns allocation/failure and real user entry addresses. The audited
//! callback executor itself is shared with the production no_std CRT.
use mac_abi::{NO_MEMORY, Status};

use crate::vec::Vec;

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
unsafe extern "C" fn callback_loop() -> !
{
    mac_traps::user_callbacks::run(&mut Storage(Vec::new()), super::failure)
}
unsafe extern "C" fn thread_return() -> !
{
    unsafe { super::__system_risc_tls_teardown_v1() };
    let mut call = mac_abi::native_threads::Call { operation: mac_abi::native_threads::EXIT,
                                                   ..Default::default() };
    if unsafe { super::client().native_thread(&mut call) }.is_err()
    {
        super::failure(mac_abi::BAD_ABI, b"native child completion refused");
    }
    // A canonical thread completion never returns to an unowned return PC.
    unsafe {
        core::arch::asm!("unimp", options(noreturn));
    }
}
