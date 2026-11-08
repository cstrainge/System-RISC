
use std::io::{ self, BufRead, Read, Write, Error, ErrorKind };

use serde_json::Value;


const MAX_HEADER_BYTES: usize = 8_192;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;


fn invalid(message: &str) -> Error { Error::new(ErrorKind::InvalidData, message) }


/**
 * LSP framing counts UTF-8 bytes, not characters. Read the complete header and body;
 * neither newline-delimited JSON nor shell output belongs on the protocol stream.
 */
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>>
{
    let mut length = None;
    let mut header_bytes = 0;
    loop
    {
        let mut line = Vec::new();
        let remaining = MAX_HEADER_BYTES.saturating_sub(header_bytes);
        let count = input.take((remaining + 1) as u64).read_until(b'\n', &mut line)?;
        if count == 0
        {
            return if header_bytes == 0 { Ok(None) }
                else { Err(Error::new(ErrorKind::UnexpectedEof, "Truncated LSP header")) };
        }
        header_bytes += count;
        if header_bytes > MAX_HEADER_BYTES { return Err(invalid("LSP header is too large")); }
        if !line.ends_with(b"\r\n") { return Err(invalid("Expected CRLF in LSP header")); }
        if line == b"\r\n" { break; }
        let line = std::str::from_utf8(&line)
            .map_err(|_| invalid("Invalid LSP header encoding"))?;
        let Some((name, value)) = line.trim_end().split_once(':') else
        { return Err(invalid("Malformed LSP header")); };
        if name.eq_ignore_ascii_case("Content-Length")
        {
            if length.is_some() { return Err(invalid("Duplicate Content-Length")); }
            length = Some(value.trim().parse::<usize>()
                .map_err(|_| invalid("Invalid Content-Length"))?);
        }
    }
    let length = length.ok_or_else(|| invalid("Missing Content-Length"))?;
    if length > MAX_MESSAGE_BYTES { return Err(invalid("LSP message is too large")); }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}


pub fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()>
{
    let body = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}
