//! Fixed-storage outcome transport; acceptance assertions remain in SDK cases.
use core::fmt::Write;
struct Text {bytes:[u8;256],length:usize}
impl core::fmt::Write for Text
{
    fn write_str(&mut self,text:&str)->core::fmt::Result
    {
        let end=self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
        self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?.copy_from_slice(text.as_bytes());
        self.length=end;Ok(())
    }
}
pub fn error(system:&mac_toolbox::System<'_>,context:&str,error:mac_toolbox::Error)
{
    let mut text=Text {bytes:[0;256],length:0};
    let _=writeln!(&mut text,"{context} status={} error={error:?}",error.status());
    if let Ok(text)=core::str::from_utf8(&text.bytes[..text.length]) {let _=system.log(text);}
}
