//! Does anything in the repository actually name this resource?
//!
//! A static, conservative scan: it looks for the `resources.json` symbol, for
//! a `(type, id)` written as a literal in the same expression (a three-line
//! window, so a wrapped `ResourceKey { kind: …, id: … }` still counts), and for
//! a resource name used as a quoted string. Anything it cannot see — an id
//! computed at runtime, a lookup driven by data, a name built at runtime — is
//! invisible to it, so "no reference found" is a prompt for review, never a
//! proof of disuse. TEMPORARY scaffolding, removed with the rest of the crate.

use std::{collections::{BTreeMap, BTreeSet, HashMap},
          path::{Path, PathBuf}};

use crate::Result;

/// One resource to look for: its key and the two ways our repo spells it.
pub struct Key
{
    pub kind:   String,
    pub id:     i16,
    pub symbol: String,
    pub name:   Option<String>
}

/// One place a resource appears to be named.
pub struct Hit
{
    pub path: String,
    pub line: usize,
    pub how:  String,
    pub test: bool
}

pub struct Usage
{
    pub hits:          BTreeMap<(String, i16), Vec<Hit>>,
    /// Types the repository looks up with an id it computes, so no static scan
    /// can rule any resource of that type out.
    pub dynamic_types: BTreeSet<String>,
    pub files:         usize
}

impl Usage
{
    pub fn of(&self, kind: &str, id: i16) -> &[Hit]
    {
        self.hits.get(&(kind.to_string(), id)).map_or(&[], Vec::as_slice)
    }
}

const SKIP: &[&str] =
    &["target", "dist", "iso", ".git", "build", "results", "vendor", "extract-list"];

/// Scan every Rust source file in `repo` for a mention of any key.
pub fn scan(repo: &Path, keys: &[Key]) -> Result<Usage>
{
    let mut symbols: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut names: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut types: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, key) in keys.iter().enumerate()
    {
        symbols.entry(key.symbol.as_str()).or_default().push(index);
        types.entry(key.kind.as_str()).or_default().push(index);
        if let Some(name) = &key.name
           && name.len() >= 6
        {
            names.entry(name.as_str()).or_default().push(index);
        }
    }
    let mut files = Vec::new();
    walk(repo, &mut files)?;
    let mut hits: BTreeMap<(String, i16), Vec<Hit>> = BTreeMap::new();
    let mut dynamic_types: BTreeSet<String> = BTreeSet::new();
    let mut counted = 0;
    for file in &files
    {
        let Ok(text) = std::fs::read_to_string(file)
        else
        {
            continue;
        };
        counted += 1;
        let relative = relative(repo, file);
        let test = is_test(&relative);
        let lines: Vec<&str> = text.lines().collect();
        let mut found: BTreeSet<(usize, usize, String)> = BTreeSet::new();
        let mut dynamic: BTreeSet<(String, usize)> = BTreeSet::new();
        for (number, line) in lines.iter().enumerate()
        {
            let tokens = identifiers(line);
            for token in &tokens
            {
                if let Some(indices) = symbols.get(token)
                {
                    for index in indices
                    {
                        found.insert((*index, number + 1, format!("symbol {token}")));
                    }
                }
            }
            for quoted in quoted_strings(line)
            {
                if let Some(indices) = types.get(quoted)
                {
                    let window = &lines[number..(number + 3).min(lines.len())];
                    let literals = integers(window);
                    let mut matched = false;
                    for literal in &literals
                    {
                        for index in indices
                        {
                            if keys[*index].id == *literal
                            {
                                matched = true;
                                found.insert((*index,
                                              number + 1,
                                              format!("literal {quoted} {literal}")));
                            }
                        }
                    }
                    let window_has_id = window.iter().any(|line| identifiers(line).contains(&"id"));
                    if !matched && window_has_id
                    {
                        dynamic_types.insert(quoted.to_string());
                        dynamic.insert((quoted.to_string(), number + 1));
                    }
                }
                if let Some(indices) = names.get(quoted)
                {
                    for index in indices
                    {
                        found.insert((*index, number + 1, format!("name \"{quoted}\"")));
                    }
                }
            }
        }
        if !dynamic.is_empty()
        {
            let all = integers(&lines);
            for (kind, line) in &dynamic
            {
                let Some(indices) = types.get(kind.as_str())
                else
                {
                    continue;
                };
                for literal in &all
                {
                    for index in indices
                    {
                        if keys[*index].id == *literal
                        {
                            found.insert((*index,
                                          *line,
                                          format!("literal {kind} {literal} (same file)")));
                        }
                    }
                }
            }
        }
        for (index, line, how) in found
        {
            let key = &keys[index];
            hits.entry((key.kind.clone(), key.id)).or_default().push(Hit { path:
                                                                               relative.clone(),
                                                                           line,
                                                                           how,
                                                                           test });
        }
    }
    Ok(Usage { hits,
               dynamic_types,
               files: counted })
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()>
{
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.path()))
                                     .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    for path in entries
    {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if path.is_dir()
        {
            if SKIP.contains(&name)
            {
                continue;
            }
            walk(&path, files)?;
        }
        else if path.extension().is_some_and(|extension| extension == "rs")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn relative(repo: &Path, file: &Path) -> String
{
    file.strip_prefix(repo).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

fn is_test(path: &str) -> bool
{
    path.split('/').any(|part| part == "tests")
    || path.rsplit('/')
           .next()
           .is_some_and(|name| name.starts_with("test") || name.ends_with("_tests.rs"))
}

fn identifiers(line: &str) -> Vec<&str>
{
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, character) in line.char_indices()
    {
        if character.is_ascii_alphanumeric() || character == '_'
        {
            start.get_or_insert(index);
        }
        else if let Some(begin) = start.take()
        {
            tokens.push(&line[begin..index]);
        }
    }
    if let Some(begin) = start
    {
        tokens.push(&line[begin..]);
    }
    tokens
}

fn quoted_strings(line: &str) -> Vec<&str>
{
    let mut strings = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('"')
    {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"')
        else
        {
            break;
        };
        strings.push(&after[..close]);
        rest = &after[close + 1..];
    }
    strings
}

/// Integer literals in a window, including the unary minus that makes a
/// resource ID negative.
fn integers(lines: &[&str]) -> Vec<i16>
{
    let mut values = Vec::new();
    for line in lines
    {
        let mut start: Option<usize> = None;
        for (index, character) in line.char_indices()
        {
            if character.is_ascii_digit()
            {
                if start.is_none()
                {
                    let before = &line[..index];
                    let trimmed = before.trim_end();
                    let begin = if trimmed.ends_with('-') { trimmed.len() - 1 } else { index };
                    start = Some(begin);
                }
            }
            else if let Some(begin) = start.take()
            {
                push_value(&line[begin..index], &mut values);
            }
        }
        if let Some(begin) = start
        {
            push_value(&line[begin..], &mut values);
        }
    }
    values
}

fn push_value(text: &str, values: &mut Vec<i16>)
{
    if let Ok(value) = text.replace('_', "").parse::<i16>()
    {
        values.push(value);
    }
}
