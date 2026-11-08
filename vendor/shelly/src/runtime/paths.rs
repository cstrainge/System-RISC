//! Shelly's search lists use colon-separated entries on System RISC.
#[cfg(not(target_os = "systemrisc"))]
pub(crate) use std::env::{join_paths, split_paths};

#[cfg(target_os = "systemrisc")]
use std::{ffi::{OsStr, OsString}, path::PathBuf};

#[cfg(target_os = "systemrisc")]
pub(crate) fn split_paths<T: AsRef<OsStr> + ?Sized>(list: &T) -> impl Iterator<Item = PathBuf> + '_
{
    list.as_ref().to_str().into_iter()
        .flat_map(|list| list.split(':'))
        .map(PathBuf::from)
}

#[cfg(target_os = "systemrisc")]
pub(crate) fn join_paths(paths: impl IntoIterator<Item = PathBuf>) -> Result<OsString, ()>
{
    let mut joined = String::new();
    for (index, path) in paths.into_iter().enumerate()
    {
        let path = path.to_str().filter(|path| !path.contains(':')).ok_or(())?;
        if index != 0 { joined.push(':'); }
        joined.push_str(path);
    }
    Ok(joined.into())
}
