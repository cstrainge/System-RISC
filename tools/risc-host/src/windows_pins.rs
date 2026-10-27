//! Metadata pins use an already unlinked private hard link. The file stays
//! physically pinned without retaining a named link inside a movable folder.
use std::{collections::BTreeMap,
          sync::{Mutex, OnceLock, Weak}};

use windows_sys::Win32::{Foundation::ERROR_LOCK_VIOLATION,
                         Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK,
                                               LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
                                               UnlockFileEx},
                         System::IO::OVERLAPPED};

use super::{PIN_DIRECTORY as DIRECTORY, *};
pub(crate) struct Pin
{
    state: Mutex<PinState>
}
pub(crate) struct PinState
{
    pub file:     File,
    pub detached: bool
}
impl Pin
{
    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, PinState>>
    {
        self.state.lock().map_err(|_| Failure(status::IO))
    }
}
type Slot = Mutex<Weak<Pin>>;
fn shared_slot(identity: wire::ObjectIdentity) -> Result<Arc<Slot>>
{
    static PINS: OnceLock<Mutex<BTreeMap<[u8; 32], Arc<Slot>>>> = OnceLock::new();
    let mut pins =
        PINS.get_or_init(|| Mutex::new(BTreeMap::new())).lock().map_err(|_| Failure(status::IO))?;
    if let Some(slot) = pins.get(identity.as_bytes())
    {
        return Ok(slot.clone());
    }
    if pins.len() >= 4096
    {
        pins.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                || slot.try_lock().map(|pin| pin.strong_count() != 0).unwrap_or(true)
            });
    }
    if pins.len() >= 4096
    {
        return Err(Failure(status::LIMIT));
    }
    let slot = Arc::new(Mutex::new(Weak::new()));
    pins.insert(*identity.as_bytes(), slot.clone());
    Ok(slot)
}
struct DataLease<'a>
{
    file:   &'a File,
    active: bool
}
#[allow(unsafe_code)]
fn data_lock(file: &File, lock: bool) -> Result<()>
{
    let mut operation = OVERLAPPED::default();
    // Cover the supported signed file extent, excluding the protocol's
    // cooperating lock byte above it. This short lease prevents an ordinary
    // external write from being hidden by our own link-count/ctime changes.
    // SAFETY: synchronous owned file, live aligned OVERLAPPED, nonblocking lock.
    let ok = unsafe {
        if lock
        {
            LockFileEx(file.as_raw_handle(),
                       LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                       0,
                       0,
                       0x80000000,
                       &mut operation)
        }
        else
        {
            UnlockFileEx(file.as_raw_handle(), 0, 0, 0x80000000, &mut operation)
        }
    };
    if ok == 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32)
        {
            return Err(Failure(status::BUSY));
        }
        return Err(error.into());
    }
    Ok(())
}
impl Drop for DataLease<'_>
{
    fn drop(&mut self)
    {
        if self.active
        {
            let _ = data_lock(self.file, false);
        }
    }
}
fn pin_directory_for(root: &Dir) -> Result<Dir>
{
    match root.symlink_metadata(DIRECTORY)
    {
        Ok(info) if !info.file_type().is_dir() => return Err(Failure(status::CORRUPT)),
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound =>
        {
            match root.create_dir(DIRECTORY)
            {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into())
            }
            if !root.symlink_metadata(DIRECTORY)?.file_type().is_dir()
            {
                return Err(Failure(status::CORRUPT));
            }
            sync_directory(root)?;
        }
        Err(e) => return Err(e.into())
    }
    pin_directory(root.open_dir(DIRECTORY)?)
}
fn name(identity: wire::ObjectIdentity) -> Result<String>
{
    let encoded: String = identity.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{encoded}-{}", unique()?.trim_start_matches(".system-risc-stage-")))
}
fn expected(name: &str) -> Result<wire::ObjectIdentity>
{
    if name.len() != 97
       || name.as_bytes()[64] != b'-'
       || !name.bytes().enumerate().all(|(at, byte)| at == 64 || byte.is_ascii_hexdigit())
    {
        return Err(Failure(status::CORRUPT));
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate()
    {
        *byte =
            u8::from_str_radix(&name[i * 2..i * 2 + 2], 16).map_err(|_| Failure(status::CORRUPT))?;
    }
    Ok(wire::ObjectIdentity::from_bytes(bytes)?)
}
pub(super) fn acquire(root: &Dir, origin: Option<(&Arc<changes::Changes>, &str)>, source: &File,
                      identity: wire::ObjectIdentity, writable: bool)
                      -> Result<Arc<Pin>>
{
    // Only this physical object's first pin/upgrade is serialized. Unrelated
    // roots hold no global mutex across I/O. Read-only metadata pins use their
    // own file object so they do not extend an owned snapshot's byte lock.
    let slot = shared_slot(identity)?;
    let mut shared = slot.lock().map_err(|_| Failure(status::IO))?;
    if let Some(pin) = shared.upgrade()
    {
        let mut state = pin.lock()?;
        if !state.detached
           && writable
           && let Some((parent, name)) = origin
        {
            state.file = detached_file(root, parent, source, name, identity)?;
            state.detached = true;
        }
        drop(state);
        return Ok(pin);
    }
    let (file, detached) = if source.metadata()?.is_dir()
    {
        (pin_directory(Dir::from_std_file(source.try_clone()?))?.into_std_file(), true)
    }
    else if writable && let Some((parent, name)) = origin
    {
        (detached_file(root, parent, source, name, identity)?, true)
    }
    else if let Some((parent, name)) = origin
    {
        let file = regular(parent.pinned_directory(), name)?;
        if object_identity(&file)? != Some(identity)
        {
            return Err(Failure(status::STALE));
        }
        (file, false)
    }
    else
    {
        (source.try_clone()?, false)
    };
    let pin = Arc::new(Pin { state: Mutex::new(PinState { file,
                                                          detached }) });
    *shared = Arc::downgrade(&pin);
    Ok(pin)
}
fn detached_file(root: &Dir, parent: &Arc<changes::Changes>, source: &File, component: &str,
                 identity: wire::ObjectIdentity)
                 -> Result<File>
{
    data_lock(source, true)?;
    let mut lease = DataLease { file:   source,
                                active: true };
    let before = parent.begin_private_metadata(component)?;
    if before.identity() != *identity.as_bytes()
    {
        return Err(Failure(status::STALE));
    }
    let pins = pin_directory_for(root)?;
    let name = name(identity)?;
    parent.pinned_directory().hard_link(component, &pins, &name)?;
    let result = (|| {
        let pin = regular(&pins, &name)?;
        if object_identity(&pin)? != Some(identity)
        {
            return Err(Failure(status::CONFLICT));
        }
        windows_unlink_private(&pins, Path::new(&name))?;
        sync_directory(&pins)?;
        parent.end_private_metadata(component, before)?;
        Ok(pin)
    })();
    if result.is_err()
    {
        // The allocated name is reserved bookkeeping. Remove only an alias of
        // the still-held source; retain a substitution or failed cleanup for
        // recovery instead of risking unrelated data.
        match regular(&pins, &name)
        {
            Ok(pin) if object_identity(&pin)? == Some(identity) =>
            {
                windows_unlink_private(&pins, Path::new(&name))?;
                sync_directory(&pins)?;
            }
            Ok(_) => return Err(Failure(status::CONFLICT)),
            Err(e) if e.0 == status::NOT_FOUND => (),
            Err(e) => return Err(e)
        }
    }
    data_lock(source, false)?;
    lease.active = false;
    result
}
/// Recover only self-identifying aliases with another surviving namespace
/// link. A lone link may be the last copy of user data and is quarantined.
pub(super) fn recover(root: &Dir) -> Result<()>
{
    match root.symlink_metadata(DIRECTORY)
    {
        Ok(info) if !info.file_type().is_dir() => return Err(Failure(status::CORRUPT)),
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into())
    }
    let pins = match root.open_dir(DIRECTORY)
    {
        Ok(directory) => pin_directory(directory)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into())
    };
    let mut count = 0;
    for entry in pins.entries()?
    {
        let entry = entry?;
        count += 1;
        if count > 1024
        {
            return Err(Failure(status::LIMIT));
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or(Failure(status::CORRUPT))?;
        let identity = expected(name)?;
        let pin = regular(&pins, name)?;
        if object_identity(&pin)? != Some(identity) || link_count(&pin.metadata()?)? < 2
        {
            return Err(Failure(status::CORRUPT));
        }
        windows_unlink_private(&pins, Path::new(name))?;
    }
    sync_directory(&pins)
}

#[cfg(test)]
mod tests
{
    use super::*;
    struct Temp(std::path::PathBuf);
    impl Temp
    {
        fn new() -> Self
        {
            let path = std::env::temp_dir().join(unique().unwrap());
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn root(&self) -> Dir
        {
            pin_directory(Dir::open_ambient_dir(&self.0, cap_std::ambient_authority()).unwrap()).unwrap()
        }
    }
    impl Drop for Temp
    {
        fn drop(&mut self)
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn pin_alias_recovery_preserves_a_lone_copy_rejects_substitution_and_can_retry()
    {
        let temp = Temp::new();
        let root = temp.root();
        std::fs::write(temp.0.join("Document"), b"original user bytes").unwrap();
        let source = regular(&root, "Document").unwrap();
        let identity = object_identity(&source).unwrap().unwrap();
        let pins = pin_directory_for(&root).unwrap();
        let alias = name(identity).unwrap();
        root.hard_link("Document", &pins, &alias).unwrap();
        recover(&root).unwrap();
        assert!(!temp.0.join(DIRECTORY).join(&alias).exists(),
                "verified redundant alias was not recovered");
        root.hard_link("Document", &pins, &alias).unwrap();
        remove_file(&root, "Document").unwrap();
        let error = recover(&root).unwrap_err();
        assert_eq!(error.0, status::CORRUPT);
        assert!(error.to_string().contains("preserve private records"),
                "missing recovery guidance: {error}");
        assert_eq!(std::fs::read(temp.0.join(DIRECTORY).join(&alias)).unwrap(),
                   b"original user bytes",
                   "recovery erased the last copy");
        pins.hard_link(&alias, &root, "Document").unwrap();
        recover(&root).unwrap();
        std::fs::write(temp.0.join(DIRECTORY).join(&alias), b"foreign substitution").unwrap();
        assert_eq!(recover(&root).unwrap_err().0, status::CORRUPT);
        assert_eq!(std::fs::read(temp.0.join(DIRECTORY).join(&alias)).unwrap(),
                   b"foreign substitution");
        remove_file(&pins, &alias).unwrap();
        root.hard_link("Document", &pins, &alias).unwrap();
        recover(&root).unwrap();
        assert_eq!(std::fs::read(temp.0.join("Document")).unwrap(), b"original user bytes");
        assert_eq!(pins.entries().unwrap().count(), 0, "successful retry left a private alias");
    }
}
