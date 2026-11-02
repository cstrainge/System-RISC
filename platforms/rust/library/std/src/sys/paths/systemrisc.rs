use mac_abi::files::{CHANGE_DIRECTORY, CURRENT_DIRECTORY, FileCall};

use crate::{ffi::{OsStr, OsString},
            fmt, io,
            marker::PhantomData,
            path::{self, Path, PathBuf},
            sys::pal::{runtime, unsupported}};

pub fn getcwd() -> io::Result<PathBuf>
{
    let mut bytes = vec![0; 4096];
    let mut call = FileCall { operation: CURRENT_DIRECTORY,
                              ..Default::default() };
    runtime::client().file(&mut call, "", &mut bytes)
                     .map_err(|cause| io::Error::from_raw_os_error(cause.status()))?;
    if call.transferred > bytes.len()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData,
                                  "native current directory exceeds its output span"));
    }
    bytes.truncate(call.transferred);
    let text = String::from_utf8(bytes).map_err(|_| {
                                           io::Error::new(io::ErrorKind::InvalidData,
                                                          "native current directory is not UTF-8")
                                       })?;
    Ok(PathBuf::from(text))
}

pub fn chdir(path: &path::Path) -> io::Result<()>
{
    let name =
        path.to_str()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "System RISC paths must be valid UTF-8")
            })?;
    let mut call = FileCall { operation: CHANGE_DIRECTORY,
                              ..Default::default() };
    runtime::client().file(&mut call, name, &mut [])
                     .map_err(|cause| io::Error::from_raw_os_error(cause.status()))
}

pub struct SplitPaths<'a>(!, PhantomData<&'a ()>);
pub struct SplitPathsRef<'a>(!, PhantomData<&'a ()>);

pub fn split_paths(_unparsed: &OsStr) -> SplitPaths<'_>
{
    panic!("unsupported")
}

pub fn split_paths_ref(_unparsed: &OsStr) -> Option<SplitPathsRef<'_>>
{
    None
}

impl<'a> Iterator for SplitPaths<'a>
{
    type Item = PathBuf;

    fn next(&mut self) -> Option<PathBuf>
    {
        self.0
    }
}

impl<'a> Iterator for SplitPathsRef<'a>
{
    type Item = &'a Path;

    fn next(&mut self) -> Option<&'a Path>
    {
        self.0
    }
}

#[derive(Debug)]
pub struct JoinPathsError;

pub fn join_paths<I, T>(_paths: I) -> Result<OsString, JoinPathsError>
    where I: Iterator<Item = T>,
          T: AsRef<OsStr>
{
    Err(JoinPathsError)
}

impl fmt::Display for JoinPathsError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        "not supported on this platform yet".fmt(f)
    }
}

impl crate::error::Error for JoinPathsError
{
}

pub fn current_exe() -> io::Result<PathBuf>
{
    unsupported()
}

pub fn temp_dir() -> PathBuf
{
    panic!("System RISC temporary directory is not configured")
}

pub fn home_dir() -> Option<PathBuf>
{
    None
}
