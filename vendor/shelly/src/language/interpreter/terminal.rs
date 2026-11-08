
use std::io::{ IsTerminal, stdout };

use crate::{ language::{ data::value::Value,
                        interpreter::{ ErrorWhat, Interpreter, InterpreterError,
                                       InterpreterResult },
                        text::location::Location },
             runtime::{ color::{ color_text, style_text },
                        readline::{ editing, try_mode_lock },
                        terminal::{ cursor_position, term_size } } };


fn terminal_pair(value: Option<(u16, u16)>) -> Value
{
    value.map_or(Value::None, |(row, column)| Value::from_array(vec![
            Value::Integer(i64::from(row)), Value::Integer(i64::from(column)),
        ]))
}


fn rgb_color(value: &Value) -> Result<(u8, u8, u8), String>
{
    let invalid = || "Color must be '#RRGGBB', 'R,G,B', or three Integers in 0..255".to_string();
    let channels: Vec<u8> = match value.underlying()
        {
            Value::String(text, _) =>
                {
                    if let Some(hex) = text.strip_prefix('#')
                    {
                        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                        { return Err(invalid()); }
                        return Ok((u8::from_str_radix(&hex[0..2], 16).unwrap(),
                                   u8::from_str_radix(&hex[2..4], 16).unwrap(),
                                   u8::from_str_radix(&hex[4..6], 16).unwrap()));
                    }
                    text.split(',').map(|channel| channel.trim().parse::<u8>())
                        .collect::<Result<_, _>>().map_err(|_| invalid())?
                },
            Value::Array(values) => values.iter().map(|value|
                {
                    let Value::Integer(value) = value.underlying() else { return Err(invalid()); };
                    u8::try_from(*value).map_err(|_| invalid())
                }).collect::<Result<_, _>>()?,
            _ => return Err(invalid()),
        };
    let [red, green, blue] = channels.as_slice() else { return Err(invalid()); };
    Ok((*red, *green, *blue))
}


fn joined_text(words: &[Value]) -> Result<String, String>
{
    words.iter().map(|word|
        {
            let Value::String(text, _) = word else
            { return Err("Terminal text must contain only Strings".to_string()); };
            Ok(text.as_str())
        }).collect::<Result<Vec<_>, _>>().map(|words| words.join(" "))
}


impl Interpreter
{
    fn terminal_write(&mut self, location: &Location, bytes: &[u8]) -> InterpreterResult<()>
    {
        if !stdout().is_terminal() { return Ok(()); }
        let busy = || InterpreterError::new(location.clone(),
            ErrorWhat::ReceiverBusy("Terminal is busy editing input".into()));
        if editing() { return Err(busy()); }
        let _mode = try_mode_lock().ok_or_else(busy)?;
        self.pipeline_stdout(location, bytes)
    }

    pub(super) fn handle_terminal_control(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let value = match args
            {
                [Value::String(command, _)] if command == "size" => terminal_pair(term_size()),
                [Value::String(command, _)] if command == "position" =>
                    {
                        if editing() { Value::None }
                        else if let Some(_mode) = try_mode_lock()
                        {
                            let mut input = Vec::new();
                            let position = cursor_position(&mut input);
                            self.queue_terminal_input(input);
                            terminal_pair(position)
                        }
                        else { Value::None }
                    },
                [Value::String(command, _), Value::Integer(row), Value::Integer(column)]
                    if command == "move" =>
                    {
                        if !(1..=65535).contains(row) || !(1..=65535).contains(column)
                        {
                            return Err(InterpreterError::new(location.clone(),
                                ErrorWhat::InvalidOperand(
                                    "Cursor row and column must be in 1..65535".into())));
                        }
                        self.terminal_write(location,
                            format!("\x1b[{};{}H", row, column).as_bytes())?;
                        Value::None
                    },
                [Value::String(command, _)] if command == "clear" =>
                    {
                        self.terminal_write(location, b"\x1b[2J\x1b[H")?;
                        Value::None
                    },
                _ => return Err(InterpreterError::new(location.clone(),
                    ErrorWhat::ArgumentMismatch("Invalid terminal control arguments".into()))),
            };
        self.last_result = Some(value);
        Ok(())
    }

    pub(super) fn handle_terminal_color(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let [color, Value::Array(words), Value::Boolean(background)] = args else
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(
                "__terminal_color expects a color, String array, and background flag".into())));
        };
        let invalid = |message| InterpreterError::new(location.clone(),
            ErrorWhat::InvalidOperand(message));
        let rgb = rgb_color(color).map_err(invalid)?;
        let text = joined_text(words).map_err(invalid)?;
        self.last_result = Some(Value::from_string(color_text(rgb, &text, self.color_mode,
                                                            *background)));
        Ok(())
    }

    pub(super) fn handle_terminal_style(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let [Value::String(style, _), Value::Array(words)] = args else
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(
                "__terminal_style expects a style name and an array of Strings".into())));
        };
        let invalid = |message| InterpreterError::new(location.clone(),
            ErrorWhat::InvalidOperand(message));
        let (code, reset) = match style.as_str()
            {
                "bold" => (1, 22), "dim" => (2, 22), "italic" => (3, 23),
                "underline" => (4, 24), "reverse" => (7, 27), "strikethrough" => (9, 29),
                _ => return Err(invalid(format!("Unknown terminal style '{}'", style))),
            };
        let text = joined_text(words).map_err(invalid)?;
        // Non-color attributes remain useful on monochrome terminals. Match is_tty's
        // stdout predicate, so redirected output receives plain text even with FORCE_COLOR.
        self.last_result = Some(Value::from_string(style_text(&text, code, reset,
                                                            stdout().is_terminal())));
        Ok(())
    }
}
