//! GUI launch environment: an owned empty initial snapshot, with process-local
//! synchronized mutations. Terminal preferences and future CLAP child launch
//! snapshots are separate; no host environment is imported.
use crate::{ffi::{OsStr, OsString},
            fmt, io,
            sync::{Arc, Mutex, OnceLock}};

type Values = Vec<(String, String)>;
static VALUES: OnceLock<Mutex<Arc<Values>>> = OnceLock::new();
fn storage() -> &'static Mutex<Arc<Values>>
{
    VALUES.get_or_init(|| Mutex::new(Arc::new(Vec::new())))
}
pub(crate) fn init()
{
    let _ = storage();
}
fn snapshot() -> Arc<Values>
{
    storage().lock().expect("System RISC environment snapshot lock is not poisoned").clone()
}
pub struct Env
{
    values: Arc<Values>,
    at:     usize
}
impl Iterator for Env
{
    type Item = (OsString, OsString);

    fn next(&mut self) -> Option<Self::Item>
    {
        let (key, value) = self.values.get(self.at)?;
        self.at += 1;
        Some((OsString::from(key.clone()), OsString::from(value.clone())))
    }

    fn size_hint(&self) -> (usize, Option<usize>)
    {
        let n = self.values.len() - self.at;
        (n, Some(n))
    }
}
impl ExactSizeIterator for Env
{
}
impl fmt::Debug for Env
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.debug_list().entries(self.values[self.at..].iter()).finish()
    }
}
pub fn env() -> Env
{
    Env { values: snapshot(),
          at:     0 }
}
pub fn getenv(name: &OsStr) -> Option<OsString>
{
    let name = name.to_str()?;
    let values = snapshot();
    values.iter().find(|(key, _)| key == name).map(|(_, value)| OsString::from(value.clone()))
}
fn valid_name(name: &OsStr) -> io::Result<&str>
{
    let name =
        name.to_str().ok_or(io::const_error!(io::ErrorKind::InvalidInput,
                                              "environment names must be UTF-8"))?;
    if name.is_empty() || name.contains('=') || name.contains('\0')
    {
        return Err(io::const_error!(io::ErrorKind::InvalidInput,
                                    "environment name is empty or contains '=' or NUL"));
    }
    Ok(name)
}
fn owned(text: &str) -> io::Result<String>
{
    let mut copy = String::new();
    copy.try_reserve_exact(text.len()).map_err(|_| {
                                           io::const_error!(io::ErrorKind::OutOfMemory,
                                                            "environment snapshot allocation \
                                                             refused")
                                       })?;
    copy.push_str(text);
    Ok(copy)
}
fn update(name: &str, value: Option<&str>) -> io::Result<()>
{
    loop
    {
        let old = snapshot();
        let existing = old.iter().find(|(key, _)| key == name);
        if existing.map(|(_, text)| text.as_str()) == value
        {
            return Ok(());
        }
        let additional = usize::from(existing.is_none() && value.is_some());
        let capacity = old.len()
                          .checked_add(additional)
                          .ok_or(io::const_error!(io::ErrorKind::OutOfMemory,
                                                  "environment snapshot capacity overflow"))?;
        let mut next = Vec::new();
        next.try_reserve_exact(capacity).map_err(|_| {
                                             io::const_error!(io::ErrorKind::OutOfMemory,
                                                              "environment snapshot allocation \
                                                               refused")
                                         })?;
        for (key, previous) in old.iter()
        {
            if key == name
            {
                if let Some(value) = value
                {
                    next.push((owned(key)?, owned(value)?));
                }
            }
            else
            {
                next.push((owned(key)?, owned(previous)?));
            }
        }
        if existing.is_none()
           && let Some(value) = value
        {
            next.push((owned(name)?, owned(value)?));
        }
        let next = Arc::try_new(next).map_err(|_| {
                                         io::const_error!(io::ErrorKind::OutOfMemory,
                                                          "environment snapshot owner allocation \
                                                           refused")
                                     })?;
        let retired = {
            let mut current =
                storage().lock().expect("System RISC environment snapshot lock is not poisoned");
            if Arc::ptr_eq(&current, &old)
            {
                Some(crate::mem::replace(&mut *current, next))
            }
            else
            {
                None
            }
        };
        // Both old arrays and refused candidates are destroyed outside the
        // environment lock; allocation/recovery never executes under it.
        if let Some(retired) = retired
        {
            drop(retired);
            return Ok(());
        }
    }
}
pub unsafe fn setenv(name: &OsStr, value: &OsStr) -> io::Result<()>
{
    let name = valid_name(name)?;
    let value = value.to_str().ok_or(io::const_error!(io::ErrorKind::InvalidInput,
                                                       "environment values must be UTF-8"))?;
    if value.contains('\0')
    {
        return Err(io::const_error!(io::ErrorKind::InvalidInput,
                                    "environment value contains NUL"));
    }
    update(name, Some(value))
}
pub unsafe fn unsetenv(name: &OsStr) -> io::Result<()>
{
    update(valid_name(name)?, None)
}
