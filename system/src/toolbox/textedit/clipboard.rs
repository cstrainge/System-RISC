//! Native editor clipboard operations. Classic private TE scrap is a separate
//! compatibility contract. The caller owns both manager guards for this call.
#![forbid(unsafe_code)]
use mac_abi::{textedit::*, *};

use super::Manager;

pub fn request(editors: &mut Manager, scrap: &mut crate::toolbox::scrap::Manager,
               call: &mut TextEditCall, input: &str, output: &mut [u8])
               -> Status
{
    if !matches!(call.operation, COPY | CUT | PASTE)
    {
        return editors.request(call, input, output);
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
                    scrap.replace_text(selected)?;
                    call.transferred = selected.len();
                    if let Some(next) = next
                    {
                        editors.apply(next)?;
                    }
                }
            }
            PASTE =>
            {
                let text = scrap.text()?;
                let next = editors.prepare_replace_selection(call.id, text)?;
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
