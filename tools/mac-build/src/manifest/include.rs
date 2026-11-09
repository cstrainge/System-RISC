//! `include` composition for a package's resource array.
//!
//! A `resources` entry `{ "include": "file.json" }` is replaced, in place, by
//! that file's own `resources` array, recursively. The path is relative to the
//! file that names it, and a `file` payload in an included file keeps that same
//! meaning: it resolves from the file that declares it, not from the package's
//! manifest. Includes may nest; a cycle, or the same `(type, id)` reached twice,
//! is an error naming the chain, because one resource has one source.

use std::{collections::BTreeMap,
          fs, io,
          path::{Component, Path, PathBuf}};

use serde_json::Value;

use crate::{Result, error,
            manifest::{Definition, Payload}};

const RESOURCES: &str = "resources";
const INCLUDE: &str = "include";

/// Replace every include in `document`'s resource array, in place and in order.
///
/// Returns the included files in the order they were read, so the caller can
/// watch them for changes.
pub(super) fn expand(path: &Path, document: &mut Value) -> Result<Vec<PathBuf>>
{
    let Some(object) = document.as_object_mut()
    else
    {
        return Ok(Vec::new());
    };
    let Some(resources) = object.remove(RESOURCES)
    else
    {
        return Ok(Vec::new());
    };
    let Value::Array(entries) = resources
    else
    {
        object.insert(RESOURCES.to_string(), resources);
        return Ok(Vec::new());
    };
    let directory = path.parent().unwrap_or(Path::new(""));
    let mut state = State { directory: directory.to_path_buf(),
                            owners:    BTreeMap::new(),
                            chain:     vec![path.to_path_buf()],
                            stack:     vec![identity(path)],
                            files:     Vec::new() };
    let mut expanded = Vec::with_capacity(entries.len());
    for entry in &entries
    {
        state.entry(entry, path, directory, &mut expanded)?;
    }
    object.insert(RESOURCES.to_string(), Value::Array(expanded));
    Ok(state.files)
}

struct State
{
    /// The package manifest's directory; included payload paths are rebased onto it.
    directory: PathBuf,
    /// First file to define each `(type, id)`, for the duplicate report.
    owners:    BTreeMap<(String, i16), PathBuf>,
    /// Declared paths of the files being expanded, outermost first.
    chain:     Vec<PathBuf>,
    /// Canonical identities of `chain`, for cycle detection.
    stack:     Vec<PathBuf>,
    /// Included files, for the caller to watch.
    files:     Vec<PathBuf>
}

impl State
{
    fn entry(&mut self, entry: &Value, file: &Path, directory: &Path, out: &mut Vec<Value>)
             -> Result<()>
    {
        if let Value::Object(map) = entry
           && map.contains_key(INCLUDE)
        {
            if map.len() != 1
            {
                return Err(error(format!("a resources entry in {} that names '{INCLUDE}' may \
                                          not also declare {}: an include is replaced in \
                                          place by the included file's resources, so move the \
                                          resource's fields into the included file",
                                         file.display(),
                                         map.keys()
                                            .filter(|key| key.as_str() != INCLUDE)
                                            .map(String::as_str)
                                            .collect::<Vec<_>>()
                                            .join(", "))));
            }
            let Some(include) = map[INCLUDE].as_str()
            else
            {
                return Err(error(format!("the '{INCLUDE}' entry in {} must be a string path \
                                          to a resources file",
                                         file.display())));
            };
            return self.include(include, file, directory, out);
        }
        let definition: Definition = serde_json::from_value(entry.clone()).map_err(|cause| {
                                         error(format!("invalid resource in {}: {cause}",
                                                       file.display()))
                                     })?;
        let key = (definition.kind.clone(), definition.id);
        if let Some(first) = self.owners.get(&key)
        {
            let subject =
                format!("resource {} {} ({})", definition.kind, definition.id, definition.symbol);
            let chain: Vec<String> =
                self.chain.iter().map(|link| link.display().to_string()).collect();
            return Err(if first == file || same(first, file)
                       {
                           error(format!("{subject} is defined twice by {}; one resource has one \
                                          source, so remove or merge one of them",
                                         file.display()))
                       }
                       else
                       {
                           error(format!("{subject} is defined by both {} and {} (include chain: \
                                          {}); one resource has one source, so remove it from \
                                          one of the two files",
                                         first.display(),
                                         file.display(),
                                         chain.join(" -> ")))
                       });
        }
        self.owners.insert(key, file.to_path_buf());
        out.push(serde_json::to_value(&rebased(definition, self.directory.as_path(), directory))?);
        Ok(())
    }

    fn include(&mut self, include: &str, file: &Path, directory: &Path, out: &mut Vec<Value>)
               -> Result<()>
    {
        let target = directory.join(include);
        let identity = identity(&target);
        if let Some(position) = self.stack.iter().position(|seen| *seen == identity)
        {
            let mut chain: Vec<String> =
                self.chain[position..].iter().map(|link| link.display().to_string()).collect();
            chain.push(target.display().to_string());
            return Err(error(format!("resource include cycle: {}", chain.join(" -> "))));
        }
        let bytes = fs::read(&target).map_err(|cause| missing(cause, &target, file))?;
        let mut document: Value = serde_json::from_slice(&bytes).map_err(|cause| {
                                      error(format!("invalid included resources file {} (named \
                                                     by {}): {cause}",
                                                    target.display(),
                                                    file.display()))
                                  })?;
        let Some(document) = document.as_object_mut()
        else
        {
            return Err(error(format!("included resources file {} (named by {}) must be a JSON \
                                      object with a '{RESOURCES}' array",
                                     target.display(),
                                     file.display())));
        };
        let Some(Value::Array(entries)) = document.remove(RESOURCES)
        else
        {
            return Err(error(format!("included resources file {} (named by {}) has no \
                                      '{RESOURCES}' array",
                                     target.display(),
                                     file.display())));
        };
        if let Some(other) = document.keys().next()
        {
            return Err(error(format!("included resources file {} (named by {}) declares \
                                      '{other}' besides '{RESOURCES}'; an include contributes \
                                      resources only, so keep the package's other settings in \
                                      its own resources.json",
                                     target.display(),
                                     file.display())));
        }
        if !self.files.contains(&target)
        {
            self.files.push(target.clone());
        }
        let directory = target.parent().unwrap_or(Path::new(""));
        self.chain.push(target.clone());
        self.stack.push(identity);
        for entry in &entries
        {
            self.entry(entry, &target, directory, out)?;
        }
        self.stack.pop();
        self.chain.pop();
        Ok(())
    }
}

/// The missing-include report. A package that includes generated extracted
/// resources has no such file in a fresh clone: that is the expected state,
/// not an anomaly, so say what produces it instead of only quoting `io::Error`.
fn missing(cause: io::Error, target: &Path, file: &Path) -> Box<dyn std::error::Error>
{
    let hint = if cause.kind() == io::ErrorKind::NotFound
    {
        "; a generated include is not checked in, so build through the project's build command, \
         which produces it from the disc the package's extract list names in the repository's iso \
         directory. Building this package directly with cargo skips that step"
    }
    else
    {
        ""
    };
    error(format!("cannot read resource include {} (named by {}): {cause}{hint}; includes are \
                   paths relative to the file that names them",
                  target.display(),
                  file.display()))
}

/// An included `file` payload resolves from the file that declares it. Rebase
/// it onto the package manifest's directory, which is what the compiler and the
/// build's change watching already use for every payload.
fn rebased(mut definition: Definition, manifest: &Path, declaring: &Path) -> Definition
{
    if let Payload::File { path } = &definition.payload
       && path.is_relative()
    {
        definition.payload = Payload::File { path: rebase(manifest, declaring, path) };
    }
    definition
}

fn rebase(manifest: &Path, declaring: &Path, path: &Path) -> PathBuf
{
    let base: Vec<Component> = manifest.components().collect();
    let from: Vec<Component> = declaring.components().collect();
    let common = base.iter().zip(&from).take_while(|(a, b)| a == b).count();
    if common == 0
    {
        return declaring.join(path);
    }
    let mut out = PathBuf::new();
    for _ in common..base.len()
    {
        out.push("..");
    }
    for component in &from[common..]
    {
        out.push(component.as_os_str());
    }
    out.push(path);
    out
}

fn identity(path: &Path) -> PathBuf
{
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same(a: &Path, b: &Path) -> bool
{
    identity(a) == identity(b)
}

