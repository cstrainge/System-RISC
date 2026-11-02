//! Ordinary std application; behavioral assertions use the public SDK.
use mac_toolbox::System;
#[path = "../../../finder_recovery_cases.rs"]
pub mod cases;
fn main() -> mac_toolbox::ToolboxResult<()>
{
    let system = System::new()?;
    match cases::finder(&system)
    {
        Ok(()) => Ok(()),
        Err(error) =>
        {
            let _ = system.log("FINDER_RECOVERY_FAIL service\n");
            Err(error)
        }
    }
}
