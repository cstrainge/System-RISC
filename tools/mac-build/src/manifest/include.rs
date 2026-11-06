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

/// Remove every include entry from a resources array without resolving it,
/// reporting the paths named. Used by a consumer that must read the manifest
/// before the includes exist; the entries are dropped because a deferred view has
/// no resources to expand them into.
pub(super) fn defer(path: &Path, document: &mut Value) -> Result<Vec<PathBuf>>
{
    let Some(object) = document.as_object_mut()
    else
    {
        return Ok(Vec::new());
    };
    let Some(Value::Array(entries)) = object.get_mut(RESOURCES)
    else
    {
        return Ok(Vec::new());
    };
    let directory = path.parent().unwrap_or(Path::new(""));
    let mut files = Vec::new();
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries.drain(..)
    {
        match entry.as_object().and_then(|map| map.get(INCLUDE))
        {
            Some(Value::String(include)) =>
            {
                files.push(directory.join(include));
            }
            Some(_) =>
            {
                return Err(error(format!("the '{INCLUDE}' entry in {} must be a string path \
                                          to a resources file",
                                         path.display())));
            }
            None => kept.push(entry)
        }
    }
    *entries = kept;
    Ok(files)
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

#[cfg(test)]
mod tests
{
    use std::fs;

    use super::*;

    fn workspace(name: &str) -> PathBuf
    {
        let root =
            std::env::temp_dir().join(format!("resource-include-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, name: &str, contents: &str)
    {
        let path = root.join(name);
        if let Some(directory) = path.parent()
        {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn resource(kind: &str, id: i16, symbol: &str, payload: &str) -> String
    {
        format!("{{\"type\":\"{kind}\",\"id\":{id},\"symbol\":\"{symbol}\",\"payload\":{payload}}}")
    }

    fn manifest(root: &Path, resources: &[String]) -> Result<Vec<(String, i16)>>
    {
        let entries: Vec<Value> =
            resources.iter().map(|entry| serde_json::from_str(entry).unwrap()).collect();
        let mut document = serde_json::json!({ "name": "test", "resources": entries });
        expand(&root.join("resources.json"), &mut document)?;
        Ok(document["resources"].as_array()
                                .unwrap()
                                .iter()
                                .map(|entry| {
                                    (entry["type"].as_str().unwrap().to_string(),
                                     entry["id"].as_i64().unwrap() as i16)
                                })
                                .collect())
    }

    #[test]
    fn expands_in_place_in_order_and_nests()
    {
        let root = workspace("order");
        write(&root,
              "nested/leaf.json",
              &format!("{{\"resources\":[{}]}}",
                       resource("STR ", 2, "B", r#"{"format":"hex","hex":"42"}"#)));
        write(&root,
              "sub/extracted.json",
              &format!("{{\"resources\":[{{\"include\":\"../nested/leaf.json\"}},{}]}}",
                       resource("STR ", 1, "A", r#"{"format":"hex","hex":"41"}"#)));
        let resources =
            manifest(&root,
                     &[String::from(r#"{"include":"sub/extracted.json"}"#),
                       resource("STR ", 3, "C", r#"{"format":"hex","hex":"43"}"#)]).unwrap();
        assert_eq!(resources,
                   vec![("STR ".to_string(), 2), ("STR ".to_string(), 1), ("STR ".to_string(), 3)]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_cycle_names_the_chain()
    {
        let root = workspace("cycle");
        write(&root, "a.json", r#"{"resources":[{"include":"b.json"}]}"#);
        write(&root, "b.json", r#"{"resources":[{"include":"a.json"}]}"#);
        let cause =
            manifest(&root, &[String::from(r#"{"include":"a.json"}"#)]).unwrap_err().to_string();
        assert!(cause.contains("resource include cycle"), "{cause}");
        assert!(cause.contains("a.json -> ") && cause.contains("b.json -> "), "{cause}");
        let self_reference = fs::read_to_string(root.join("a.json")).unwrap();
        assert!(self_reference.contains("b.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_duplicate_across_the_boundary_names_both_files_and_the_chain()
    {
        let root = workspace("duplicate");
        write(&root,
              "extracted.json",
              &format!("{{\"resources\":[{}]}}",
                       resource("DITL", 128, "SHARED", r#"{"format":"hex","hex":"00"}"#)));
        let cause = manifest(&root,
                             &[String::from(r#"{"include":"extracted.json"}"#),
                               resource("DITL", 128, "SHARED", r#"{"format":"hex","hex":"00"}"#)])
                    .unwrap_err()
                    .to_string();
        assert!(cause.contains("DITL 128"), "{cause}");
        assert!(cause.contains("extracted.json") && cause.contains("resources.json"), "{cause}");
        assert!(cause.contains("include chain"), "{cause}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_include_names_the_file_that_named_it()
    {
        let root = workspace("missing");
        let cause = manifest(&root,
                             &[String::from(r#"{"include":"extracted_resources.json"}"#)])
                    .unwrap_err()
                    .to_string();
        assert!(cause.contains("cannot read resource include"), "{cause}");
        assert!(cause.contains("extracted_resources.json") && cause.contains("resources.json"),
                "{cause}");
        assert!(cause.contains("build through the project's build command")
                && cause.contains("iso directory"),
                "{cause}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn included_file_payloads_resolve_from_the_declaring_file()
    {
        let root = workspace("payload");
        write(&root, "extracted/asset.bin", "hello");
        write(&root,
              "extracted/resources.json",
              &format!("{{\"resources\":[{}]}}",
                       resource("STR ", 1, "A", r#"{"format":"file","path":"asset.bin"}"#)));
        let mut document =
            serde_json::json!({ "resources": [ {"include": "extracted/resources.json"} ] });
        expand(&root.join("resources.json"), &mut document).unwrap();
        let path = document["resources"][0]["payload"]["path"].as_str().unwrap();
        assert_eq!(Path::new(path), Path::new("extracted").join("asset.bin"), "{path}");
        assert_eq!(fs::read(root.join("resources.json").parent().unwrap().join(path)).unwrap(),
                   b"hello");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_deferred_read_skips_the_include_and_reports_it()
    {
        let root = workspace("deferred");
        write(&root,
              "extracted_resources.json",
              &format!("{{\"resources\":[{}]}}",
                       resource("STR ", 1, "GENERATED", r#"{"format":"hex","hex":"41"}"#)));
        write(&root,
              "resources.json",
              r#"{"archive":true,"name":"Test","file_type":"RSRC","creator":"TEST",
                  "role":"code_resource","abi_version":1,"description":"deferred fixture",
                  "resources":[{"include":"extracted_resources.json"},
                               {"type":"STR ","id":2,"symbol":"OURS","payload":{"format":"hex","hex":"42"}}]}"#);
        let path = root.join("resources.json");
        let deferred = crate::Manifest::read_deferred(&path).unwrap();
        assert_eq!(deferred.resources.len(),
                   1,
                   "the include contributes nothing to a deferred view");
        assert_eq!(deferred.resources[0].symbol, "OURS");
        assert_eq!(deferred.include_files(),
                   [root.join("extracted_resources.json")].as_slice(),
                   "the deferred file is reported so the caller knows what it did not see");
        // A deferred read works when the include is absent, which is the state a
        // fresh checkout is in before extraction has run.
        fs::remove_file(root.join("extracted_resources.json")).unwrap();
        let again = crate::Manifest::read_deferred(&path).unwrap();
        assert_eq!(again.resources.len(), 1);
        assert!(crate::Manifest::read(&path).is_err(),
                "the complete read still requires the include");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_package_manifest_compiles_the_included_resources_in_order()
    {
        let root = workspace("read");
        write(&root,
              "extracted_resources.json",
              &format!("{{\"resources\":[{}]}}",
                       resource("STR ", 1, "FIRST", r#"{"format":"hex","hex":"41"}"#)));
        write(&root,
              "resources.json",
              r#"{"archive":true,"name":"Test","file_type":"RSRC","creator":"TEST",
                  "role":"code_resource","abi_version":1,"description":"include fixture",
                  "resources":[{"include":"extracted_resources.json"}]}"#);
        let manifest = crate::Manifest::read(&root.join("resources.json")).unwrap();
        assert_eq!(manifest.resources.len(), 1);
        assert_eq!(manifest.resources[0].symbol, "FIRST");
        assert_eq!(manifest.include_files(),
                   [root.join("extracted_resources.json")].as_slice(),
                   "the included file is reported so the build can watch it");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_included_file_contributes_resources_only()
    {
        let root = workspace("shape");
        write(&root,
              "other.json",
              r#"{"name":"nope","resources":[{"type":"STR ","id":1,"symbol":"A","payload":{"format":"hex","hex":"41"}}]}"#);
        let cause = manifest(&root, &[String::from(r#"{"include":"other.json"}"#)]).unwrap_err()
                                                                                   .to_string();
        assert!(cause.contains("contributes resources only"), "{cause}");
        fs::remove_dir_all(root).unwrap();
    }
}
