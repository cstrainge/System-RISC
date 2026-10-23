//! Internal TextEdit uses CR. Preserve the document's external newline style.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum LineEnding
{
    #[default]
    Lf,
    Cr,
    CrLf
}
pub fn decode(bytes: &mut [u8]) -> (usize, LineEnding)
{
    let mut at: usize = 0;
    let mut out = 0;
    let mut style = None;
    while at < bytes.len()
    {
        let byte = bytes[at];
        at += 1;
        bytes[out] = match byte
        {
            b'\r' =>
            {
                let ending = if bytes.get(at) == Some(&b'\n')
                {
                    at += 1;
                    LineEnding::CrLf
                }
                else
                {
                    LineEnding::Cr
                };
                style.get_or_insert(ending);
                b'\r'
            }
            b'\n' =>
            {
                style.get_or_insert(LineEnding::Lf);
                b'\r'
            }
            other => other
        };
        out += 1;
    }
    (out, style.unwrap_or_default())
}
pub fn encode_chunk(input: &str, style: LineEnding, output: &mut [u8]) -> Option<usize>
{
    let mut at: usize = 0;
    for byte in input.bytes()
    {
        let bytes: &[u8] = match (byte, style)
        {
            (b'\r', LineEnding::CrLf) => b"\r\n",
            (b'\r', LineEnding::Lf) => b"\n",
            _ => core::slice::from_ref(&byte)
        };
        output.get_mut(at..at.checked_add(bytes.len())?)?.copy_from_slice(bytes);
        at += bytes.len();
    }
    Some(at)
}
