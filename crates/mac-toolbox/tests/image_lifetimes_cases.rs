//! Image lifetime assertions execute through the SDK in real native processes.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{Error, System, resources::ImageScope};

pub fn string(system: &System<'_>, scope: ImageScope, expected: &str) -> Result<(), Error>
{
    let mut output = [0; 64];
    if system.resources().image(scope).string(2000, &mut output)? != expected
    {
        return Err(Error::BadAbi);
    }
    Ok(())
}
pub fn stale(system: &System<'_>, scope: ImageScope) -> Result<(), Error>
{
    let error = system.resources()
                      .image(scope)
                      .get(mac_toolbox::ResourceKey::from_bytes(*b"STR ", 2000))
                      .err()
                      .ok_or(Error::BadAbi)?;
    if error != Error::NotFound
    {
        return Err(Error::BadAbi);
    }
    if system.resources().image(scope).type_count() != Err(Error::NotFound)
    {
        return Err(Error::BadAbi);
    }
    Ok(())
}
