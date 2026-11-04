//! Applying the platform's Rust source patches.
//!
//! The SDK build used to shell out to `git apply`. That is a dependency on a
//! program the build otherwise never needs, and under WSL it fails for an
//! unrelated reason: the worktree's `.git` file holds a Windows drive path that
//! WSL git reads as relative. The patches are plain unified diffs against
//! `rust-src`, so they are applied here instead, in memory, and every file is
//! checked before any file is written.

use std::{collections::BTreeMap,
          fs,
          path::{Path, PathBuf}};

use mac_build::{Result, error};

/// One hunk of a diff, with the line numbers the diff declares.
struct Hunk
{
    old_start: usize,
    old_len:   usize,
    /// How many lines the hunk produces. The new-side start is not kept: the
    /// hunk is required to apply at exactly the position the diff names, so the
    /// only thing left to check is that its two lengths agree with its body.
    new_len:   usize,
    /// `(' ', line)`, `('-', line)` or `('+', line)`, in file order.
    lines:     Vec<(char, String)>
}

impl Hunk
{
    /// The lines the hunk expects to find, in order.
    fn before(&self) -> Vec<&str>
    {
        self.lines.iter().filter(|(kind, _)| *kind != '+').map(|(_, line)| line.as_str()).collect()
    }

    /// The lines the hunk replaces them with.
    fn after(&self) -> Vec<&str>
    {
        self.lines.iter().filter(|(kind, _)| *kind != '-').map(|(_, line)| line.as_str()).collect()
    }
}

struct FileDiff
{
    path:  String,
    hunks: Vec<Hunk>
}

/// Apply every patch in order, refusing the whole set if any hunk does not
/// match. Returns the files that changed.
pub fn apply_all(root: &Path, patches: &[PathBuf]) -> Result<Vec<String>>
{
    // Applied in memory across all patches, so a later patch sees the result of
    // an earlier one exactly as `git apply` in sequence would.
    let mut pending: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut originals: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    for patch in patches
    {
        let text = fs::read_to_string(patch).map_err(|e| {
                                                error(format!("cannot read patch {}: {e}",
                                                              patch.display()))
                                            })?;
        for diff in parse(&text, patch)?
        {
            let existing = match pending.get(&diff.path)
            {
                Some(lines) => Some(lines.clone()),
                None => read(&root.join(&diff.path))?.map(split)
            };
            originals.entry(diff.path.clone()).or_insert_with(|| existing.clone());
            let updated = match &existing
            {
                Some(lines) => apply(&diff, lines.clone(), patch)?,
                None =>
                {
                    return Err(error(format!("{}: patch target '{}' does not exist",
                                             patch.display(),
                                             diff.path)));
                }
            };
            pending.insert(diff.path, updated);
        }
    }
    // Every hunk matched, so write. A file whose content is unchanged is left
    // alone rather than touched, which keeps a warm SDK build warm.
    let mut changed = Vec::new();
    for (path, lines) in pending
    {
        if originals.get(&path).and_then(|lines| lines.clone()).as_deref() == Some(&lines[..])
        {
            continue;
        }
        let target = root.join(&path);
        if let Some(parent) = target.parent()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, lines.join("\n") + "\n")?;
        changed.push(path);
    }
    Ok(changed)
}

fn read(path: &Path) -> Result<Option<String>>
{
    match fs::read_to_string(path)
    {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(format!("cannot read {}: {e}", path.display())))
    }
}

fn split(text: String) -> Vec<String>
{
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    // A trailing newline produces one empty final element; drop it so the
    // line list matches what the diff talks about.
    if lines.last().is_some_and(|line| line.is_empty())
    {
        lines.pop();
    }
    lines
}

fn parse(text: &str, patch: &Path) -> Result<Vec<FileDiff>>
{
    let lines: Vec<&str> =
        text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    let mut diffs = Vec::new();
    let mut index = 0;
    while index < lines.len()
    {
        let Some(path) = lines[index].strip_prefix("+++ ")
        else
        {
            index += 1;
            continue;
        };
        let path = path.split('\t').next().unwrap_or(path).trim();
        let path = path.strip_prefix("b/").unwrap_or(path).to_string();
        index += 1;
        let mut hunks = Vec::new();
        while index < lines.len()
        {
            let line = lines[index];
            if line.starts_with("--- ") || line.starts_with("diff --git")
            {
                break;
            }
            if let Some(header) = line.strip_prefix("@@ ")
            {
                index += 1;
                let (hunk, next) = hunk(header, &lines, index, patch, &path)?;
                index = next;
                hunks.push(hunk);
            }
            else
            {
                index += 1;
            }
        }
        if hunks.is_empty()
        {
            return Err(error(format!("{}: no hunks for '{path}'", patch.display())));
        }
        diffs.push(FileDiff { path,
                              hunks });
    }
    if diffs.is_empty()
    {
        return Err(error(format!("{}: no file diffs found", patch.display())));
    }
    Ok(diffs)
}

fn hunk(header: &str, lines: &[&str], mut index: usize, patch: &Path, path: &str)
        -> Result<(Hunk, usize)>
{
    let (old, new) = header.split_once(" @@")
                           .map(|(ranges, _)| ranges)
                           .unwrap_or(header)
                           .split_once(' ')
                           .ok_or_else(|| {
                               error(format!("{}: malformed hunk header '@@ {header}'",
                                             patch.display()))
                           })?;
    let (old_start, old_len) = range(old, patch)?;
    let (_, new_len) = range(new, patch)?;
    // The declared ranges, not the shape of the following lines, decide where
    // the body ends. The next file's `---`/`+++` header lines also start with
    // `-`/`+`, so scanning until a non-body character appears would swallow
    // them and count them as hunk lines.
    let mut body = Vec::new();
    let mut old_seen = 0usize;
    let mut new_seen = 0usize;
    while index < lines.len() && (old_seen < old_len || new_seen < new_len)
    {
        let line = lines[index];
        let Some(kind) = line.chars().next()
        else
        {
            // An empty line inside a hunk is a context line whose leading space
            // was stripped in transit.
            body.push((' ', String::new()));
            old_seen += 1;
            new_seen += 1;
            index += 1;
            continue;
        };
        match kind
        {
            ' ' =>
            {
                body.push((' ', line[1..].to_string()));
                old_seen += 1;
                new_seen += 1;
                index += 1;
            }
            '-' =>
            {
                body.push(('-', line[1..].to_string()));
                old_seen += 1;
                index += 1;
            }
            '+' =>
            {
                body.push(('+', line[1..].to_string()));
                new_seen += 1;
                index += 1;
            }
            '\\' =>
            {
                // `\ No newline at end of file`; rust-src ends every file with a
                // newline, so there is nothing to restore.
                index += 1;
            }
            _ => break
        }
    }
    let found = body.iter().filter(|(kind, _)| *kind != '+').count();
    if found != old_len
    {
        return Err(error(format!("{}: hunk for '{path}' declares {old_len} old lines but \
                                  supplies {found}",
                                 patch.display())));
    }
    let produced = body.iter().filter(|(kind, _)| *kind != '-').count();
    if produced != new_len
    {
        return Err(error(format!("{}: hunk for '{path}' declares {new_len} new lines but \
                                  produces {produced}",
                                 patch.display())));
    }
    Ok((Hunk { old_start,
               old_len,
               new_len,
               lines: body },
        index))
}

fn range(text: &str, patch: &Path) -> Result<(usize, usize)>
{
    let (start, len) = match text.split_once(',')
    {
        Some((start, len)) => (start, len),
        None => (text, "1")
    };
    let parse = |value: &str| -> Result<usize> {
        // The range carries the side's sign: `-32,7` on the old side and
        // `+32,7` on the new one.
        value.trim().trim_start_matches(['-', '+']).parse().map_err(|_| {
                                                               error(format!("{}: cannot read \
                                                                              hunk range '{text}'",
                                                                             patch.display()))
                                                           })
    };
    Ok((parse(start)?, parse(len)?))
}

fn apply(diff: &FileDiff, mut lines: Vec<String>, patch: &Path) -> Result<Vec<String>>
{
    // Each applied hunk shifts everything after it by the difference between
    // the lengths it removes and adds.
    let mut shift: isize = 0;
    for hunk in &diff.hunks
    {
        let expected = (hunk.old_start as isize - 1 + shift).max(0) as usize;
        let before = hunk.before();
        let after = hunk.after();
        // A hunk that does not apply where the diff says it does is a fact to
        // report, not a position to search for. Guessing would splice the hunk
        // into a different function, and the result would compile code nobody
        // wrote.
        if !matches(&lines, &before, expected)
        {
            return Err(error(format!("{}: hunk at line {} of '{}' does not match the source",
                                     patch.display(),
                                     hunk.old_start,
                                     diff.path)));
        }
        lines.splice(expected..expected + before.len(), after.iter().map(|line| line.to_string()));
        shift += hunk.new_len as isize - hunk.old_len as isize;
    }
    Ok(lines)
}

/// Whether the context lines appear at exactly this position.
fn matches(lines: &[String], before: &[&str], at: usize) -> bool
{
    at + before.len() <= lines.len()
    && before.iter().enumerate().all(|(offset, line)| lines[at + offset] == *line)
}
