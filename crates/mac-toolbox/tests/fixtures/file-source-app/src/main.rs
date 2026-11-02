use mac_toolbox::System;
#[path = "../../../file_source_cases.rs"]
mod cases;
fn main() -> mac_toolbox::ToolboxResult<()>
{
    let system = System::new()?;
    let _ = system.log("DESKTOP_READY: public backing-file fixture\n");
    match cases::check(&system)
    {
        Ok(()) =>
        {
            let _ = system.log("FILE_SOURCE_PASS\n");
            Ok(())
        }
        Err(error) =>
        {
            let _ = system.log("FILE_SOURCE_FAIL\n");
            Err(error)
        }
    }
}
