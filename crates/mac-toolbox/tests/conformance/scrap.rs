//! Original System7.6.1 Scrap contract through the public native userspace SDK.
//! TEXT is ASCII in the compatibility trace, so native UTF-8 byte lengths equal
//! original MacRoman lengths. Unicode and Pascal coverage are native extensions.
use mac_toolbox::{Error, System,
                  scrap::{FlavorInfo, NO_TYPE_ERR, TEXT}};

use super::{Failure, require};
#[derive(Clone, Copy)]
pub struct Observation
{
    pub action: &'static str,
    pub status: i32,
    pub size:   u32,
    pub offset: u32,
    pub bytes:  &'static [u8]
}
mod scrap_oracle;
pub use scrap_oracle::OBSERVATIONS;
fn oracle(action: &str) -> Result<&'static Observation, Failure>
{
    OBSERVATIONS.iter()
                .find(|o| o.action == action)
                .ok_or(Failure::Mismatch("missing original Scrap observation"))
}
fn info(system: &System<'_>, action: &'static str, generation: u16) -> Result<(), Failure>
{
    let expected = oracle(action)?;
    let actual = system.scrap().info()?;
    require(actual.size == expected.size, "original scrapSize")?;
    require(actual.count == generation, "PutScrap/GetScrap leave scrapCount unchanged")
}
fn clear(system: &System<'_>, action: &'static str) -> Result<u16, Failure>
{
    let before = system.scrap().info()?.count;
    system.scrap().clear()?;
    let after = system.scrap().info()?;
    require(after.count != before, "ZeroScrap changes scrapCount")?;
    require(after.size == oracle(action)?.size && after.flavors == 0,
            "ZeroScrap empties all flavors")?;
    Ok(after.count)
}
fn get(system: &System<'_>, action: &'static str, kind: [u8; 4], generation: u16,
       flavor_index: usize)
       -> Result<(), Failure>
{
    let expected = oracle(action)?;
    let mut output = [0xa5; 64];
    let result = if kind == TEXT
    {
        system.scrap().text(&mut output).map(str::len)
    }
    else
    {
        system.scrap().read(kind, 0, &mut output)
    };
    if expected.status < 0
    {
        require(result == Err(Error(expected.status)), "original GetScrap missing-flavor error")?;
        require(output == [0xa5; 64], "missing flavor preserves caller storage")?;
    }
    else
    {
        let length = result?;
        require(length == expected.status as usize && output[..length] == *expected.bytes,
                "original GetScrap payload and result length")?;
        require(output[length..].iter().all(|&b| b == 0xa5), "GetScrap output tail")?;
        require(system.scrap().flavor_at(flavor_index)?
                == FlavorInfo { kind,
                                length: length as u32,
                                offset: expected.offset },
                "original GetScrap payload offset and format order")?;
    }
    info(system, action, generation)
}
pub fn original_scrap(system: &System<'_>) -> Result<usize, Failure>
{
    require(OBSERVATIONS.len() == 19, "original Scrap capture count")?;
    // System boot clipboard contents differ. Start at the original first clear;
    // compare count changes, never absolute counts or undocumented increments.
    clear(system, "zero_first")?;
    let generation = clear(system, "zero_second")?;
    get(system, "missing_empty", *b"NONE", generation, 0)?;
    system.scrap().put_text("abc")?;
    info(system, "put_odd_TEXT", generation)?;
    get(system, "get_TEXT", TEXT, generation, 0)?;
    let query = system.scrap().flavor_at(0)?;
    require(query.length == oracle("query_TEXT_nil")?.status as u32
            && query.offset == oracle("query_TEXT_nil")?.offset,
            "original GetScrap nil destination query")?;
    info(system, "query_TEXT_nil", generation)?;
    system.scrap().put(*b"PICT", b"1234")?;
    info(system, "put_even_PICT", generation)?;
    get(system, "get_PICT", *b"PICT", generation, 1)?;
    get(system, "missing_flavor", *b"NONE", generation, 0)?;
    system.scrap().put_text("DEFGH")?;
    info(system, "put_repeat_TEXT", generation)?;
    get(system, "repeat_get_TEXT", TEXT, generation, 0)?;
    get(system, "repeat_get_PICT", *b"PICT", generation, 1)?;
    system.scrap().put(*b"TEST", b"")?;
    info(system, "put_zero_TEST", generation)?;
    get(system, "get_zero_TEST", *b"TEST", generation, 3)?;
    system.scrap().put_text("")?;
    info(system, "put_zero_TEXT", generation)?;
    get(system, "get_zero_TEXT", TEXT, generation, 0)?;
    require(system.scrap().info()?.flavors == 5, "repeated and zero-length records retained")?;
    require(system.scrap().flavor_at(2)?
            == FlavorInfo { kind:   TEXT,
                            length: 5,
                            offset: 32 },
            "duplicate TEXT remains an appended record")?;
    require(system.scrap().flavor_at(4)?
            == FlavorInfo { kind:   TEXT,
                            length: 0,
                            offset: 54 },
            "zero TEXT remains an appended record")?;
    let generation = clear(system, "zero_final")?;
    get(system, "get_after_zero", TEXT, generation, 0)?;
    require(NO_TYPE_ERR == -102, "public missing-flavor error mapping")?;
    Ok(18)
}

/// Native Unicode and safe borrowed-buffer extension, separate from the Roman
/// original profile. Both native and RV64 call the same real resident manager.
pub fn native_scrap(system: &System<'_>) -> Result<usize, Failure>
{
    use mac_toolbox::{BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED};
    system.scrap().clear()?;
    const TEXT_VALUE: &str = "RISCy café — 中文 🦀\rLine two\0tail";
    system.scrap().put_text(TEXT_VALUE)?;
    let mut output = [0xa5; 128];
    require(system.scrap().text(&mut output)? == TEXT_VALUE, "full native Unicode clipboard")?;
    require(output[TEXT_VALUE.len()..].iter().all(|&b| b == 0xa5), "native text output tail")?;
    let mut short = [0xa5; 2];
    require(system.scrap().text(&mut short) == Err(Error(NO_MEMORY)), "text does not truncate")?;
    require(short == [0xa5; 2], "short text output preserved")?;
    require(system.scrap().put(TEXT, b"bypass") == Err(Error(BAD_ARGUMENT)),
            "binary TEXT put rejected")?;
    require(system.scrap().read(TEXT, 0, &mut short) == Err(Error(BAD_ARGUMENT)),
            "binary TEXT read rejected")?;
    let before = system.scrap().info()?;
    let mut call = mac_abi::scrap::ScrapCall { operation: mac_abi::scrap::PUT_TEXT,
                                               ..Default::default() };
    require(system.client().scrap(&mut call, &[0xff, 0xfe], &mut []) == Err(Error(BAD_ARGUMENT)),
            "invalid native UTF-8 rejected at gate")?;
    require(system.scrap().info()? == before, "failed UTF-8 put preserves clipboard")?;
    const BINARY: &[u8] = &[0, 0xff, 0xfe, 0x80, 0, 0x41, 0x0d];
    system.scrap().put(*b"BINR", BINARY)?;
    let mut binary = [0xa5; 10];
    require(system.scrap().read(*b"BINR", 2, &mut binary)? == 5 && binary[..5] == BINARY[2..],
            "native partial arbitrary binary read")?;
    require(binary[5..] == [0xa5; 5], "binary output tail")?;
    binary.fill(0xa5);
    require(system.scrap().read(*b"BINR", BINARY.len(), &mut binary)? == 0, "binary EOF")?;
    require(system.scrap().read(*b"BINR", BINARY.len() + 1, &mut binary)
            == Err(Error(BAD_ARGUMENT)),
            "binary read beyond EOF")?;
    require(binary == [0xa5; 10], "failed binary output preserved")?;
    require(system.scrap().flavor_at(2) == Err(Error(NOT_FOUND)), "record enumeration end")?;
    let before = system.scrap().info()?;
    require(system.scrap().load() == Err(Error(NOT_SUPPORTED)), "disk scrap load unsupported")?;
    require(system.scrap().unload() == Err(Error(NOT_SUPPORTED)), "disk scrap unload unsupported")?;
    require(system.scrap().info()? == before, "unsupported disk operations preserve state")?;
    system.scrap().clear()?;
    Ok(17)
}

/// Paired Pascal/MacRoman convenience traps convert into the exact same native
/// UTF-8 manager. This is not the counted-text original A-line Scrap ABI.
pub fn legacy_text(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<usize, Failure>
{
    use mac_abi::{BAD_ARGUMENT, NO_MEMORY, scrap::*, traps::PSTRING_SCRAP};
    fn raw(gate: mac_abi::TrapGate, call: &mut ScrapCall) -> i64
    {
        unsafe {
            gate(PSTRING_SCRAP, core::ptr::from_mut(call).cast(), core::mem::size_of::<ScrapCall>())
        }
    }
    system.scrap().clear()?;
    let text = b"\x04Caf\x8e";
    let mut call = ScrapCall { operation: PUT_TEXT,
                               input: text.as_ptr(),
                               input_length: text.len(),
                               ..Default::default() };
    require(raw(gate, &mut call) == 0, "Pascal Scrap put")?;
    let mut output = [0xa5; 768];
    require(system.scrap().text(&mut output)? == "Café", "Pascal input becomes native UTF-8")?;
    output.fill(0xa5);
    call.operation = READ_TEXT;
    call.input = core::ptr::null();
    call.input_length = 0;
    call.output = output.as_mut_ptr();
    call.capacity = output.len();
    require(raw(gate, &mut call) == 0 && &output[..call.transferred] == text,
            "Pascal Scrap output roundtrip")?;
    require(output[call.transferred..].iter().all(|&b| b == 0xa5), "Pascal output tail")?;
    output.fill(0xa5);
    call.capacity = 2;
    require(raw(gate, &mut call) == NO_MEMORY as i64, "short Pascal output error")?;
    require(output == [0xa5; 768], "short Pascal output preserved")?;
    let before = system.scrap().info()?;
    let malformed = b"\x02x";
    call.operation = PUT_TEXT;
    call.input = malformed.as_ptr();
    call.input_length = malformed.len();
    call.output = core::ptr::null_mut();
    call.capacity = 0;
    require(raw(gate, &mut call) == BAD_ARGUMENT as i64, "malformed Pascal input rejected")?;
    require(system.scrap().info()? == before, "malformed Pascal input preserves clipboard")?;
    call.operation = PUT;
    call.kind = *b"PICT";
    require(raw(gate, &mut call) == BAD_ARGUMENT as i64,
            "Pascal convenience rejects binary operations")?;
    system.scrap().clear()?;
    system.scrap().put_text("中文 🦀")?;
    call.operation = READ_TEXT;
    call.input = core::ptr::null();
    call.input_length = 0;
    call.output = output.as_mut_ptr();
    call.capacity = output.len();
    require(raw(gate, &mut call) == BAD_ARGUMENT as i64, "unrepresentable native text rejected")?;
    require(output == [0xa5; 768], "unrepresentable output preserved")?;
    system.scrap().clear()?;
    let long_ascii = [b'A'; 256];
    system.scrap().put_text(core::str::from_utf8(&long_ascii).map_err(|_| {
                                                                  Failure::Mismatch("ASCII fixture")
                                                              })?)?;
    require(raw(gate, &mut call) == BAD_ARGUMENT as i64,
            "native text over255 MacRoman bytes rejected")?;
    require(output == [0xa5; 768], "overlong Pascal output preserved")?;
    system.scrap().clear()?;
    let mut maximum = [0xde; 256];
    maximum[0] = 255;
    call.operation = PUT_TEXT;
    call.input = maximum.as_ptr();
    call.input_length = maximum.len();
    call.output = core::ptr::null_mut();
    call.capacity = 0;
    require(raw(gate, &mut call) == 0, "maximum Pascal input")?;
    let native = system.scrap().text(&mut output)?;
    require(native.len() == 765 && native.chars().all(|character| character == 'ﬁ'),
            "maximum MacRoman expands to765 native UTF-8 bytes")?;
    output.fill(0xa5);
    call.operation = READ_TEXT;
    call.input = core::ptr::null();
    call.input_length = 0;
    call.output = output.as_mut_ptr();
    call.capacity = output.len();
    require(raw(gate, &mut call) == 0 && output[..call.transferred] == maximum,
            "maximum Pascal output roundtrip")?;
    require(output[call.transferred..].iter().all(|&b| b == 0xa5), "maximum Pascal output tail")?;
    system.scrap().clear()?;
    Ok(12)
}
