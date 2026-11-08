
use crate::language::text::span::SourceSpan;


#[derive(Clone, Debug)]
pub struct InterpolationReference
{
    pub span: SourceSpan,
    pub name: String,
    pub error: Option<&'static str>,
}


/**
 * Find references without looking up their values. Both execution and tooling use
 * these name boundaries; spans refer to the decoded string supplied by the caller.
 */
pub fn interpolation_references(text: &str, escaped_dollars: &[usize])
    -> Vec<InterpolationReference>
{
    let mut references = Vec::new();
    let mut characters = text.char_indices().peekable();
    while let Some((start, character)) = characters.next()
    {
        if character != '$' || escaped_dollars.binary_search(&start).is_ok() { continue; }
        let mut name = String::from("$");
        let mut error = None;
        if characters.peek().is_some_and(|(_, c)| *c == '{')
        {
            characters.next();
            let mut closed = false;
            for (_, character) in characters.by_ref()
            {
                if character == '}' { closed = true; break; }
                name.push(character);
            }
            if !closed || name.len() == 1
            { error = Some("Expected a variable name and closing '}' in string interpolation."); }
        }
        else
        {
            while let Some(&(_, character)) = characters.peek()
            {
                if character == ':'
                {
                    let mut probe = characters.clone();
                    probe.next();
                    if    probe.next().is_some_and(|(_, c)| c == ':')
                       && probe.peek().is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_')
                    {
                        characters.next();
                        characters.next();
                        name.push_str("::");
                        continue;
                    }
                }
                if !character.is_alphanumeric() && character != '_' { break; }
                name.push(character);
                characters.next();
            }
            if name.len() == 1 { continue; }
        }
        if error.is_none() && name.contains('.')
        {
            for (index, part) in name.split('.').enumerate()
            {
                let part = if index == 0 { part } else { part.strip_prefix("::").unwrap_or(part) };
                let part = if index == 0 || part.starts_with('$')
                    { part.strip_prefix('$').unwrap_or(part) } else { part };
                if    part.is_empty()
                   || part.chars().next().is_some_and(|c| c.is_numeric())
                   || !part.split("::").all(|segment| !segment.is_empty()
                        && segment.chars().all(|c| c.is_alphanumeric() || c == '_'))
                { error = Some("Expected a variable and field names in string interpolation."); }
            }
        }
        if error.is_none() && name.contains('=')
        { error = Some("Invalid variable name: '=' is not allowed."); }
        let end = characters.peek().map_or(text.len(), |(offset, _)| *offset);
        references.push(InterpolationReference { span: SourceSpan::new(start, end), name, error });
    }
    references
}
