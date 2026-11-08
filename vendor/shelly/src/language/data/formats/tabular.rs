
use std::{ collections::VecDeque, mem::take };

use csv::{ ReaderBuilder, WriterBuilder };

use crate::language::data::value::{ Value, Executable };


fn delimiter(format: &str) -> u8
{
    match format { "csv" => b',', "tsv" => b'\t', "ssv" => b' ', _ => unreachable!() }
}


pub(super) fn encode(format: &str, value: &Value) -> Result<String, String>
{
    let Value::Array(values) = value.underlying() else
        { return Err("Tabular encoding requires an array row or an array of rows".into()); };
    let mut writer = WriterBuilder::new().delimiter(delimiter(format)).flexible(true)
        .from_writer(Vec::new());
    let rows: Vec<_> = if values.iter().any(|value| matches!(value.underlying(), Value::Array(_)))
        { values.iter().collect() } else { vec![value] };
    for row in rows
    {
        let Value::Array(fields) = row.underlying() else
            { return Err("Every tabular row must be an Array".into()); };
        let fields = fields.iter().map(|field| match field.underlying()
            {
                Value::String(text, Executable::No) => Ok(text.clone()),
                Value::Integer(_) | Value::Float(_, _) | Value::Boolean(_) | Value::None =>
                    Ok(field.as_text()),
                value => Err(format!("Tabular fields cannot contain {}", value.type_name())),
            }).collect::<Result<Vec<_>, _>>()?;
        writer.write_record(&fields).map_err(|error| error.to_string())?;
    }
    let bytes = writer.into_inner().map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}


pub(super) fn decode(format: &str, text: &str) -> Result<Value, String>
{
    let mut frames = RecordDecoder::new(format);
    frames.push(text.as_bytes());
    frames.finish();
    let first = frames.next()?.ok_or_else(|| "Tabular decoding requires one row".to_string())?;
    if frames.next()?.is_some()
    { return Err("Tabular from_string converts one row; use a pipeline for multiple rows".into()); }
    decode_record(format, &first)
}


fn decode_record(format: &str, record: &str) -> Result<Value, String>
{
    // Only TSV skips RFC 4180 double-quote quoting: it has none in its spec, so a
    // literal quote in data stays literal. CSV keeps strict quoting, and SSV keeps
    // the symmetric quote handling its round-trip tests pin.
    let mut reader = ReaderBuilder::new().delimiter(delimiter(format)).has_headers(false)
        .flexible(true).quoting(!matches!(format, "tsv")).from_reader(record.as_bytes());
    let record = reader.records().next().ok_or_else(|| "Missing tabular row".to_string())?
        .map_err(|error| error.to_string())?;
    Ok(Value::from_array(record.iter()
        .map(|field| Value::from_string(field.to_string())).collect()))
}


// Incremental record framing is shared by direct factories and pipeline decoding.
// Only one logical record is retained, including quoted delimiters and embedded newlines.
// Quoting follows the same rule as decode_record: TSV treats double quotes as
// ordinary data, CSV and SSV honor them.
pub struct RecordDecoder
{
    pending: VecDeque<u8>,
    record: Vec<u8>,
    delimiter: u8,
    quoting: bool,
    quoted: bool,
    after_quote: bool,
    field_start: bool,
    skip_lf: bool,
    eof: bool,
}


impl RecordDecoder
{
    pub fn new(format: &str) -> Self
    {
        Self { pending: VecDeque::new(), record: Vec::new(), delimiter: delimiter(format),
            quoting: !matches!(format, "tsv"),
            quoted: false, after_quote: false, field_start: true, skip_lf: false, eof: false }
    }

    pub fn push(&mut self, bytes: &[u8]) { self.pending.extend(bytes); }
    pub fn finish(&mut self) { self.eof = true; }

    pub fn next(&mut self) -> Result<Option<String>, String>
    {
        while let Some(byte) = self.pending.pop_front()
        {
            if self.skip_lf
            {
                self.skip_lf = false;
                if byte == b'\n' { continue; }
            }
            if self.quoted
            {
                self.record.push(byte);
                if byte == b'"' { self.quoted = false; self.after_quote = true; }
                continue;
            }
            if self.after_quote && byte == b'"'
            {
                self.record.push(byte);
                self.quoted = true;
                self.after_quote = false;
                continue;
            }
            if self.after_quote && !matches!(byte, b'\r' | b'\n') && byte != self.delimiter
            { return Err("Unexpected character after a quoted field".into()); }
            if matches!(byte, b'\r' | b'\n')
            {
                self.skip_lf = byte == b'\r';
                self.after_quote = false;
                self.field_start = true;
                if self.record.is_empty() { continue; }
                let record = take(&mut self.record);
                return String::from_utf8(record).map(Some).map_err(|error| error.to_string());
            }
            if byte == b'"' && self.quoting
            {
                if !self.field_start { return Err("Quote inside an unquoted field".into()); }
                self.quoted = true;
            }
            self.field_start = byte == self.delimiter;
            self.after_quote = false;
            self.record.push(byte);
        }
        if self.eof
        {
            if self.quoted { return Err("Unterminated quoted field".into()); }
            if !self.record.is_empty()
            {
                let record = take(&mut self.record);
                return String::from_utf8(record).map(Some).map_err(|error| error.to_string());
            }
        }
        Ok(None)
    }

    pub fn ended(&self) -> bool { self.eof && self.pending.is_empty() && self.record.is_empty() }
}
