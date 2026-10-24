//! System-owned drag sessions and checked transport boundary.
mod manager;
pub(crate) mod lifetime;
pub use manager::{Manager, Notice};
pub mod native;

/// Freeze source mount policy with the offered references, not on each motion.
pub(crate) fn source_read_only(files: &mut crate::toolbox::files::Manager,
                               call: &mac_abi::drag::Call, input: &[u8])
                               -> Result<bool, mac_abi::Status>
{
    use mac_abi::{drag::*, *};
    let file_items = call.operation == FILE_ITEMS;
    if !file_items && !(call.operation == OFFER && call.kind == *b"fref")
    {
        return Ok(false);
    }
    if input.is_empty()
       || input.len() % FILE_ITEM_BYTES != 0
       || file_items && input.len() / FILE_ITEM_BYTES > MAX_ITEMS
       || !file_items && input.len() != FILE_ITEM_BYTES
    {
        return Err(BAD_ARGUMENT);
    }
    let mut checked = [0u32; crate::toolbox::files::MAX_VOLUMES];
    let mut count = 0;
    let mut read_only = false;
    for bytes in input.chunks_exact(FILE_ITEM_BYTES)
    {
        let volume = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        if !checked[..count].contains(&volume)
        {
            if count == checked.len()
            {
                return Err(NO_MEMORY);
            }
            let mut request = files::FileCall { operation: files::VOLUME_INFO,
                                                volume,
                                                ..Default::default() };
            files.request(&mut request, "", &mut [])?;
            read_only |= request.volume_info.read_only != 0;
            checked[count] = volume;
            count += 1;
        }
    }
    Ok(read_only)
}

/// A folder/trash response declares a filesystem-backed group operation. Keep
/// admission shared by the live desktop and SDK conformance service table.
pub(crate) fn validate_target(files: &mut crate::toolbox::files::Manager,
                              call: &mut mac_abi::drag::Call)
                              -> Result<(), mac_abi::Status>
{
    use mac_abi::{drag::*, *};
    if call.operation != ACCEPT || call.flags & 1 == 0 || call.volume == 0
    {
        return Ok(());
    }
    let mut request = files::FileCall { operation: files::VOLUME_INFO,
                                        volume: call.volume,
                                        ..Default::default() };
    files.request(&mut request, "", &mut [])?;
    if call.flags & TARGET_EJECT != 0
    {
        if call.parent != files::ROOT_ID
           || call.flags & (TARGET_TRASH | TARGET_ICON_LAYOUT | TARGET_APPLICATION | ACCEPT_COPY)
              != 0
           || call.flags & ACCEPT_MOVE == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let result = if request.volume_info.startup != 0
        {
            Err(ACCESS_DENIED)
        }
        else
        {
            files.check_provider_unmount(call.volume).map_err(|e| {
                                                         if e == BAD_ARGUMENT
                                                         {
                                                             NOT_SUPPORTED
                                                         }
                                                         else
                                                         {
                                                             e
                                                         }
                                                     })
        };
        if let Err(error) = result
        {
            call.flags = 0;
            call.error = error;
        }
        return Ok(());
    }
    if call.flags & TARGET_APPLICATION != 0
    {
        if call.parent <= files::ROOT_ID
           || call.flags & (TARGET_TRASH | TARGET_ICON_LAYOUT | ACCEPT_MOVE) != 0
           || call.flags & ACCEPT_COPY == 0
        {
            return Err(BAD_ARGUMENT);
        }
        return Ok(()); // Opening documents never writes the application's mount.
    }
    if call.flags & TARGET_ICON_LAYOUT != 0
    {
        if call.parent < files::ROOT_ID
           || call.flags & (TARGET_TRASH | ACCEPT_COPY) != 0
           || call.flags & ACCEPT_MOVE == 0
        {
            return Err(BAD_ARGUMENT);
        }
        // The Drag Manager verifies the frozen file-reference source folder.
        return Ok(());
    }
    if request.volume_info.read_only != 0
    {
        call.flags = 0;
        call.error = READ_ONLY;
    }
    else if !files.supports_icon_moves(call.volume)?
    {
        call.flags &= !ACCEPT_MOVE;
        if call.flags & (ACCEPT_COPY | ACCEPT_MOVE) == 0
        {
            call.flags = 0;
            call.error = NOT_SUPPORTED;
        }
    }
    Ok(())
}
