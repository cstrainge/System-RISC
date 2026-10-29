//! Native editor clipboard operations. Classic private TE scrap is a separate
//! compatibility contract. The caller owns both manager guards for this call.
#![forbid(unsafe_code)]
use alloc::{borrow::Cow, string::String};

use mac_abi::{textedit::*, *};

use super::Manager;

// Native Unicode clipboard text uses LF; the classic editor model uses CR.
// Conversion belongs to this boundary, not the shared Scrap or host transport.
fn from_clipboard(text: &str) -> Result<Cow<'_, str>, Status>
{
    if !text.as_bytes().contains(&b'\n')
    {
        return Ok(Cow::Borrowed(text));
    }
    let pairs = text.as_bytes().windows(2).filter(|pair| *pair == b"\r\n").count();
    let length = text.len() - pairs;
    if length > MAX_BYTES as usize
    {
        return Err(NO_MEMORY);
    }
    let mut result = String::new();
    result.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    let mut characters = text.chars().peekable();
    while let Some(ch) = characters.next()
    {
        if ch == '\r' && characters.peek() == Some(&'\n')
        {
            characters.next();
        }
        result.push(if ch == '\n' { '\r' } else { ch });
    }
    Ok(Cow::Owned(result))
}

fn to_clipboard(text: &str) -> Result<Cow<'_, str>, Status>
{
    if !text.as_bytes().contains(&b'\r')
    {
        return Ok(Cow::Borrowed(text));
    }
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| NO_MEMORY)?;
    result.extend(text.chars().map(|ch| if ch == '\r' { '\n' } else { ch }));
    Ok(Cow::Owned(result))
}

pub fn request(editors: &mut Manager, scrap: &mut crate::toolbox::scrap::Manager,
               call: &mut TextEditCall, input: &str, output: &mut [u8],
               fonts: &crate::toolbox::quickdraw::fonts::FontManager)
               -> Status
{
    if !matches!(call.operation, COPY | CUT | PASTE)
    {
        return editors.request_with_fonts(call, input, output, fonts);
    }
    call.transferred = 0;
    let result = (|| -> Result<(), Status> {
        match call.operation
        {
            COPY | CUT =>
            {
                let selected = editors.selected_text(call.id)?;
                // A native menu Copy with no selection leaves the clipboard.
                if !selected.is_empty()
                {
                    let exported = to_clipboard(selected)?;
                    // Every allocation for a cut finishes before publishing
                    // the clipboard or replacing the editor. With the editor
                    // guard held, the prepared ID cannot become stale here.
                    let next = if call.operation == CUT
                    {
                        Some(editors.prepare_delete(call.id)?)
                    }
                    else
                    {
                        None
                    };
                    scrap.replace_text(exported.as_ref())?;
                    call.transferred = exported.len();
                    if let Some(next) = next
                    {
                        editors.apply(next)?;
                    }
                }
            }
            PASTE =>
            {
                let text = from_clipboard(scrap.text()?)?;
                let next = editors.prepare_replace_with_fonts(call.id, text.as_ref(), fonts)?;
                editors.apply(next)?;
                call.transferred = text.len();
            }
            _ => unreachable!()
        }
        call.info = editors.inspect(call.id).ok_or(NOT_FOUND)?.info();
        Ok(())
    })();
    result.map_or_else(|error| error, |()| OK)
}
