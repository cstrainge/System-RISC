//! Shared native fixture edge. No host-linked service or guest kernel hook.
use core::fmt::{self, Write};

use mac_driver::{Error, Provider};
use mac_scheduler_interface::control::{self, Reply};

pub enum Failure
{
    Observation(&'static str, i32),
    Control(mac_driver::scheduler::ControlError)
}
impl Failure
{
    pub fn status(&self) -> i32
    {
        match self
        {
            Self::Observation(_, status) => *status,
            Self::Control(cause) => cause.status()
        }
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Observation(stage, status) => write!(f, "{stage}: status={status}"),
            Self::Control(cause) => cause.fmt(f)
        }
    }
}
impl From<mac_driver::scheduler::ControlError> for Failure
{
    fn from(cause: mac_driver::scheduler::ControlError) -> Self
    {
        Self::Control(cause)
    }
}
pub type Result<T> = core::result::Result<T, Failure>;
pub fn require(observed: bool, stage: &'static str) -> Result<()>
{
    if observed { Ok(()) } else { Err(Failure::Observation(stage, mac_abi::BAD_ABI)) }
}
pub fn service<T>(stage: &'static str, result: mac_driver::Result<T>) -> Result<T>
{
    result.map_err(|Error(status)| Failure::Observation(stage, status))
}
/// Preserve resident typed failures and their actual nonempty diagnostic. Do
/// not classify by prose or log from the policy callback.
pub fn refusal(provider: Provider, operation: u32, input: &[u8], expected: i32,
               stage: &'static str)
               -> Result<()>
{
    let mut output = [0; control::MAX_REPLY];
    let length = service(stage, provider.synchronous(operation, input, &mut output))?;
    match Reply::decode(&output[..length])
    {
        Ok(Reply::Failure { status,
                            cause })
            if status == expected && !cause.is_empty() =>
        {
            log(format_args!("SCHEDULER_REFUSAL stage={stage} status={status} cause={cause}\n"));
            Ok(())
        }
        _ => Err(Failure::Observation(stage, mac_abi::BAD_ABI))
    }
}
/// A short output is refused before the operation has an effect. Live buffers
/// remain valid; no fake pointer is passed and subsequent valid status proves
/// the original grant is still usable.
pub fn short_reply(provider: Provider, operation: u32, input: &[u8]) -> Result<()>
{
    let mut output = [0xa5; control::MIN_REPLY - 1];
    require(provider.synchronous(operation, input, &mut output)
            == Err(Error(mac_abi::BAD_ARGUMENT)),
            "short SCTL reply was not refused")?;
    require(output.iter().all(|byte| *byte == 0xa5), "short SCTL reply wrote caller buffer")
}
pub fn startup_status(provider: Provider) -> Result<(u32, i32)>
{
    let mut output = [0; mac_module::lifecycle::REPLY_BYTES];
    let length = service("MLIF status",
                         provider.synchronous(mac_module::lifecycle::STATUS, &[], &mut output))?;
    require((8..=8 + mac_module::lifecycle::MAX_CAUSE).contains(&length), "MLIF status length")?;
    let phase =
        u32::from_le_bytes(output[..4].try_into()
                                      .map_err(|_| {
                                          Failure::Observation("MLIF phase", mac_abi::BAD_ABI)
                                      })?);
    let status =
        i32::from_le_bytes(output[4..8].try_into()
                                       .map_err(|_| {
                                           Failure::Observation("MLIF status", mac_abi::BAD_ABI)
                                       })?);
    require(status <= 0 && phase <= mac_module::lifecycle::REFUSED, "MLIF phase/status")?;
    require(core::str::from_utf8(&output[8..length]).is_ok_and(|text| {
                                                        !text.chars().any(char::is_control)
                                                    }),
            "MLIF cause UTF-8")?;
    if status < 0
    {
        let cause =
            core::str::from_utf8(&output[8..length]).map_err(|_| {
                                                        Failure::Observation("MLIF cause UTF-8",
                                                                             mac_abi::BAD_ABI)
                                                    })?;
        log(format_args!("SCHEDULER_MLIF_STATUS phase={phase} status={status} cause={cause}\n"));
    }
    Ok((phase, status))
}
/// One bounded synchronous LOG per milestone/refusal, never per scheduling
/// decision. The real native trap borrows this live stack buffer.
pub fn log(arguments: fmt::Arguments<'_>)
{
    struct Text
    {
        bytes:  [u8; 768],
        length: usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, text: &str) -> fmt::Result
        {
            if text.len() > self.bytes.len() - self.length
            {
                return Err(fmt::Error);
            }
            self.bytes[self.length..self.length + text.len()].copy_from_slice(text.as_bytes());
            self.length += text.len();
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 768],
                          length: 0 };
    if text.write_fmt(arguments).is_err()
    {
        text.length = 0;
        let _ = text.write_str("SCHEDULER_FIXTURE_FAILURE diagnostic overflow\n");
    }
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.length };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: this is the existing fixture's synchronous native LOG boundary;
    // exact call/text storage remains live until the trap returns.
    unsafe {
        core::arch::asm!("ecall", in("a7") mac_abi::traps::LOG,
                         inlateout("a0") result,
                         in("a1") core::mem::size_of_val(&call), clobber_abi("C"));
    }
    let _ = result;
}
