use core::fmt::Write;

use mac_toolbox::*;

pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8]) -> Result<&'b str, Status>
{
    system.resources().application().indexed_string(2000, index, output).map_err(|e| e.0)
}

pub struct Text<const N: usize>
{
    bytes:  [u8; N],
    length: usize
}
impl<const N: usize> Default for Text<N>
{
    fn default() -> Self
    {
        Self { bytes:  [0; N],
               length: 0 }
    }
}
impl<const N: usize> Write for Text<N>
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
        let output = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
        output.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
impl<const N: usize> Text<N>
{
    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }
}
