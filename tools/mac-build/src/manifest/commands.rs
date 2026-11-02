//! One command table supplies both MCMD bytes and Rust dispatch constants.
use std::collections::BTreeMap;

use super::{Manifest, MenuItem, Payload, fourcc};
use crate::{Result, error};

pub(super) struct Commands
{
    pub menus:   BTreeMap<i16, Vec<u32>>,
    pub symbols: BTreeMap<String, u32>
}

struct Command
{
    value:    u32,
    code:     String,
    location: String,
    explicit: Option<(String, String)>
}

impl Commands
{
    pub fn parse(manifest: &Manifest) -> Result<Self>
    {
        let mut menus = BTreeMap::new();
        let mut commands = BTreeMap::<u32, Command>::new();
        for (resource_index, definition) in manifest.resources.iter().enumerate()
        {
            let Payload::Menu { items,
                                .. } = &definition.payload
            else
            {
                continue;
            };
            if definition.kind != "MENU"
            {
                return Err(error("menu payload requires MENU"));
            }
            let mut values = Vec::with_capacity(items.len());
            for (item_index, item) in items.iter().enumerate()
            {
                let location = format!("resources[{resource_index}] (MENU {}, \
                                        {}).payload.items[{item_index}]",
                                       definition.id, definition.symbol);
                let (code, symbol) = match item
                {
                    MenuItem::Text(_) => (None, None),
                    MenuItem::Definition(item) =>
                    {
                        (item.command.as_deref(), item.command_symbol.as_deref())
                    }
                };
                let value = code.map(fourcc)
                                .transpose()
                                .map_err(|cause| error(format!("{location}.command: {cause}")))?
                                .map_or(0, u32::from_be_bytes);
                values.push(value);
                if let Some(symbol) = symbol
                {
                    if value == 0
                    {
                        return Err(error(format!("{location}.command_symbol requires a \
                                                  nonzero command")));
                    }
                    if !valid_symbol(symbol)
                    {
                        return Err(error(format!("{location}.command_symbol {symbol:?} must \
                                                  start with A-Z and contain only A-Z, 0-9 or \
                                                  underscores")));
                    }
                }
                if value == 0
                {
                    continue;
                }
                let command = commands.entry(value).or_insert_with(|| {
                                                       Command { value,
                                                                 code: code.unwrap_or_default()
                                                                           .to_owned(),
                                                                 location: location.clone(),
                                                                 explicit: None }
                                                   });
                if let Some(symbol) = symbol
                {
                    if let Some((previous, previous_location)) = &command.explicit
                       && previous != symbol
                    {
                        return Err(error(format!("ambiguous command_symbol for {:?} \
                                                  (0x{value:08x}): {previous:?} at \
                                                  {previous_location} and {symbol:?} at \
                                                  {location}; use one symbol for this command",
                                                 command.code)));
                    }
                    command.location = location.clone();
                    command.explicit = Some((symbol.to_owned(), location));
                }
            }
            menus.insert(definition.id, values);
        }
        let mut named = BTreeMap::<String, Command>::new();
        for command in commands.into_values()
        {
            let symbol = match &command.explicit
            {
                Some((symbol, _)) => symbol.clone(),
                None =>
                {
                    automatic_symbol(&command.code).ok_or_else(|| {
                                                       error(format!("command {:?} (0x{:08x}) at \
                                                                      {} needs an explicit \
                                                                      command_symbol: automatic \
                                                                      names require ASCII \
                                                                      letters/digits and space, \
                                                                      -, _, . or / separators",
                                                                     command.code,
                                                                     command.value,
                                                                     command.location))
                                                   })?
                }
            };
            if let Some(previous) = named.get(&symbol)
            {
                return Err(error(format!("command symbol {symbol} collides: {:?} (0x{:08x}) \
                                          at {} and {:?} (0x{:08x}) at {}; set a distinct \
                                          command_symbol",
                                         previous.code,
                                         previous.value,
                                         previous.location,
                                         command.code,
                                         command.value,
                                         command.location)));
            }
            named.insert(symbol, command);
        }
        Ok(Self { menus,
                  symbols: named.into_iter()
                                .map(|(symbol, command)| (symbol, command.value))
                                .collect() })
    }
}

fn valid_symbol(symbol: &str) -> bool
{
    symbol.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
    && symbol.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn automatic_symbol(code: &str) -> Option<String>
{
    let mut symbol = String::new();
    let mut useful = false;
    for byte in code.trim_matches(' ').bytes()
    {
        symbol.push(match byte
                    {
                        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' =>
                        {
                            useful = true;
                            byte.to_ascii_uppercase() as char
                        }
                        b' ' | b'-' | b'_' | b'.' | b'/' => '_',
                        _ => return None
                    });
    }
    if !useful
    {
        return None;
    }
    if !symbol.as_bytes()[0].is_ascii_uppercase()
    {
        symbol.insert_str(0, "COMMAND_");
    }
    Some(symbol)
}
