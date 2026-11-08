pub mod json;
mod mapping;
mod tabular;

pub use crate::language::data::formats::mapping::map_value;
pub use crate::language::data::formats::tabular::RecordDecoder;

use crate::language::data::value::{ Executable, Value };


pub const FACTORIES: &[(&str, &str, bool)] = &[
        ("String::from_csv", "csv", true), ("csv::from_string", "csv", false),
        ("String::from_tsv", "tsv", true), ("tsv::from_string", "tsv", false),
        ("String::from_ssv", "ssv", true), ("ssv::from_string", "ssv", false),
    ];


pub fn encode(format: &str, value: &Value) -> Result<Value, String>
{
    let text = match format
        {
            "csv" | "tsv" | "ssv" => tabular::encode(format, value)?,
            _ => return Err(format!("Unsupported conversion format '{}'", format)),
        };
    Ok(Value::from_string(text))
}


pub fn decode(format: &str, value: &Value) -> Result<Value, String>
{
    let Value::String(text, Executable::No) = value.underlying() else
        { return Err("A format decoder requires a String".into()); };
    match format
    {
        "csv" | "tsv" | "ssv" => tabular::decode(format, text),
        _ => Err(format!("Unsupported conversion format '{}'", format)),
    }
}
