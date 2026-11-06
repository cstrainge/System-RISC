//! Named pipes as Finder items. Opening one is `connect`; it is never a launch.
//! The item has no fork, so no application is associated with it and no
//! document is handed to one.
#![forbid(unsafe_code)]
use mac_toolbox::{Error, System, TypeCode, files};

/// The reserved type and creator a named pipe carries in the Pipes folder. The
/// creator must match too, so an unrelated file that happens to carry the type
/// is not treated as a pipe.
const PIPE: [u8; 4] = *b"PIPE";

pub fn candidate(entry: &files::CatalogInfo) -> bool
{
    !entry.is_directory()
    && entry.file_type == TypeCode::new(PIPE)
    && entry.creator == TypeCode::new(PIPE)
}

/// Connect, then release. The Finder has no stream of its own to feed, so the
/// writer is closed immediately: the gesture's meaning is that connecting
/// happened and nothing was launched. An application with data to write keeps
/// its writer instead. A refused connect is reported to the caller as the
/// action failure it is.
pub fn open(system: &System<'_>, entry: &files::CatalogInfo) -> Result<(), Error>
{
    let writer = system.files().open_pipe(entry.volume, entry.id)?;
    let writer = system.pipes().adopt_writer(writer)?;
    system.log(&format!("FINDER_PIPE_CONNECTED name={} volume={} cnid={}",
                        entry.name(),
                        entry.volume.0,
                        entry.id.0))?;
    writer.close()
}
