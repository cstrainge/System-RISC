//! Bounded module JSON transactions use the same real File Manager and block
//! filesystem as applications. No file I/O runs under the module registry lock.
use mac_module::preferences as wire;

use super::*;

impl Manager
{
    pub(crate) fn module_preference_revision(&self) -> Result<u64, Status>
    {
        self.volumes
            .iter()
            .find_map(|(_, v)| {
                match v
                {
                    Mounted::Block(mounted) if mounted.info.startup != 0 =>
                    {
                        Some(mounted.catalog_revision)
                    }
                    _ => None
                }
            })
            .ok_or(NOT_FOUND)
    }

    pub(crate) fn module_preferences(&mut self, owner: &OwnerRef, name: &str,
                                     expected: Option<Option<&[u8]>>, new: &[u8],
                                     output: &mut [u8])
                                     -> Result<usize, Status>
    {
        if name.is_empty()
           || name == "."
           || name == ".."
           || name.contains(['/', ':', '\0'])
           || name.chars().any(char::is_control)
           || new.len() > wire::MAX_BYTES
           || output.len() < wire::REPLY_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        let volume = self.volumes
                         .iter()
                         .find(|(_, v)| v.info().is_some_and(|i| i.startup != 0))
                         .map(|(id, _)| id)
                         .ok_or(NOT_FOUND)?;
        let mut current = [0; wire::MAX_BYTES];
        let (parent, cnid, length) =
            self.with_volume(volume, |v| {
                    let system = io(v.blessed_folder())?;
                    let prefs = required(io(v.lookup(system.id, b"Preferences"))?)?;
                    let Some(file) = io(v.lookup(prefs.id, name.as_bytes()))?
                    else
                    {
                        return Ok((prefs.id, 0, 0));
                    };
                    let mut fork = file.data.ok_or(CORRUPT_DATA)?;
                    let length = usize::try_from(fork.length).map_err(|_| CORRUPT_DATA)?;
                    if length > current.len()
                    {
                        return Err(CORRUPT_DATA);
                    }
                    if io(v.read(&mut fork, 0, &mut current[..length]))? != length
                    {
                        return Err(IO_ERROR);
                    }
                    Ok((prefs.id, file.id, length))
                })?;
        let actual = (cnid != 0).then_some(&current[..length]);
        let Some(expected) = expected
        else
        {
            output[0] = u8::from(cnid != 0);
            output[1..1 + length].copy_from_slice(&current[..length]);
            return Ok(1 + length);
        };
        if actual != expected
        {
            return Err(SCAN_STALE);
        }
        // Compare, admission, staging and commit share this actual Files guard.
        // The ordinary writer owns its unique FDWR cleanup ticket on refusal.
        let label = owner.owner().incarnation();
        let when = crate::machine::clock::now()?.mac_microseconds().ok_or(BAD_ARGUMENT)?;
        let mut call = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                       volume,
                                       parent,
                                       expected_cnid: cnid,
                                       when,
                                       file_type: *b"JSON",
                                       creator: *b"MACS",
                                       length: new.len(),
                                       ..Default::default() };
        self.write_request_for(label, owner, &mut call, name, &[])?;
        call.operation = APPEND_DATA_WRITE;
        let result = self.write_request(label, &mut call, "", new).and_then(|()| {
                                                                      call.operation =
                                                                          COMMIT_DATA_WRITE;
                                                                      self.write_request(label,
                                                                                         &mut call,
                                                                                         "",
                                                                                         &[])
                                                                  });
        if let Err(original) = result
        {
            call.operation = ABORT_DATA_WRITE;
            if let Err(cleanup) = self.write_request(label, &mut call, "", &[])
            {
                serial!("MODULE_PREFERENCES_CLEANUP_RETAINED handle={} original={} cleanup={}",
                        call.handle, original, cleanup);
                return Err(ROLLBACK_FAILED);
            }
            return Err(original);
        }
        Ok(0)
    }
}
