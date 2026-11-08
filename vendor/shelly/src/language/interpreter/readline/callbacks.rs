
use reedline::Suggestion;

use crate::{
    language::{data::value::Value, interpreter::Interpreter, text::location::Location},
    runtime::readline::{Answer, Edit, Fault},
};

use super::{
    Editor, Result,
    values::{self, field, record, string},
};

impl Interpreter
{
    fn readline_edit(&self, edit: &Edit) -> Value
    {
        let (start, end) = edit.word();
        let span = record(
            self,
            "Span",
            vec![Value::Integer(start as i64), Value::Integer(end as i64)],
        );
        record(
            self,
            "Edit",
            vec![
                Value::from_string(edit.text.clone()),
                Value::Integer(edit.cursor as i64),
                Value::Integer(edit.revision as i64),
                Value::from_string(edit.text[..edit.cursor].into()),
                Value::from_string(edit.text[edit.cursor..].into()),
                span,
                Value::from_string(edit.text[start..edit.cursor].into()),
            ],
        )
    }
    pub(super) fn readline_callback(
        &mut self,
        location: &Location,
        editor: &Editor,
        hook: &str,
        edit: &Edit,
    ) -> Result<Answer>
    {
        if edit.text.len() > editor.limits.input
            || edit.cursor > edit.text.len()
            || !edit.text.is_char_boundary(edit.cursor)
        {
            return Err(Fault::new("LimitError", "Input exceeds input limits").into());
        }
        let callback = match hook
        {
            "complete" => &editor.complete,
            "validate" => &editor.validate,
            _ => &editor.highlight,
        };
        let input = self.readline_edit(edit);
        let value = self.readline_call(
            location,
            callback.clone(),
            vec![input],
            Some(&editor.limits),
        )?;
        match hook
        {
            "validate" =>
            {
                let kind = match values::enum_name(field(&value, "kind"))
                {
                    Some("Ready") => 0,
                    Some("More") => 1,
                    Some("Reject") => 2,
                    _ => return Err(Fault::new("CallbackError", "Invalid Validation").into()),
                };
                let message =
                    values::optional_string(field(&value, "message"))?.unwrap_or_default();
                if message.len() > 16384
                {
                    return Err(
                        Fault::new("CallbackError", "Validation message is too large").into(),
                    );
                }
                let span = if matches!(field(&value, "span").underlying(), Value::None)
                {
                    None
                }
                else
                {
                    Some(values::span(field(&value, "span"), &edit.text)?)
                };
                Ok(Answer::Validate {
                    kind,
                    message,
                    span,
                })
            }
            "complete" =>
            {
                let Value::Array(items) = value.underlying()
                else
                {
                    return Err(
                        Fault::new("CallbackError", "Completion must return an Array").into(),
                    );
                };
                if items.len() > editor.limits.completions
                {
                    return Err(Fault::new("CallbackError", "Too many completions").into());
                }
                let (start, end) = edit.word();
                let default = reedline::Span::new(start, end);
                let mut replacement = None;
                let mut suggestions = Vec::new();
                let mut bytes = 0;
                for item in items.iter()
                {
                    let plain = string(item);
                    let value =
                        plain
                            .or_else(|| string(field(item, "value")))
                            .ok_or_else(|| {
                                Fault::new("CallbackError", "Completion value must be String")
                            })?;
                    let span = if plain.is_some()
                        || matches!(field(item, "replace").underlying(), Value::None)
                    {
                        default
                    }
                    else
                    {
                        values::span(field(item, "replace"), &edit.text)?
                    };
                    if span.start > edit.cursor
                        || span.end < edit.cursor
                        || replacement.is_some_and(|previous| previous != span)
                    {
                        return Err(Fault::new(
                            "CallbackError",
                            "Completions must share a range containing cursor",
                        )
                        .into());
                    }
                    replacement = Some(span);
                    let display = values::optional_string(field(item, "display"))?;
                    let description = values::optional_string(field(item, "description"))?;
                    let display =
                        crate::runtime::readline::visible(display.as_deref().unwrap_or(value));
                    let description =
                        description.map(|text| crate::runtime::readline::visible(&text));
                    bytes +=
                        value.len() + display.len() + description.as_ref().map_or(0, String::len);
                    if bytes > editor.limits.completion_bytes
                    {
                        return Err(
                            Fault::new("CallbackError", "Completion bytes exceed limit").into()
                        );
                    }
                    let append = matches!(
                        field(item, "append_space").underlying(),
                        Value::Boolean(true)
                    ) && !edit.text[span.end..].starts_with(char::is_whitespace);
                    suggestions.push(Suggestion {
                        value: value.into(),
                        span,
                        display_override: Some(display),
                        description,
                        append_whitespace: append,
                        ..Suggestion::default()
                    });
                }
                Ok(Answer::Complete(suggestions))
            }
            _ =>
            {
                let Value::Array(items) = value.underlying()
                else
                {
                    return Err(
                        Fault::new("CallbackError", "Highlight must return an Array").into(),
                    );
                };
                if items.len() > editor.limits.spans
                {
                    return Err(Fault::new("CallbackError", "Too many highlight spans").into());
                }
                let mut end = 0;
                let mut spans = Vec::new();
                for item in items.iter()
                {
                    let span = values::span(field(item, "span"), &edit.text)?;
                    if span.start < end
                    {
                        return Err(Fault::new(
                            "CallbackError",
                            "Highlight spans overlap or are unsorted",
                        )
                        .into());
                    }
                    end = span.end;
                    spans.push((span, values::style(field(item, "style"))?));
                }
                Ok(Answer::Highlight(spans))
            }
        }
    }
}
