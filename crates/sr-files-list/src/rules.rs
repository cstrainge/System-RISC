//! Which icon a file, folder or volume gets, and which entries a file list
//! shows. Every view of files asks here, so the answers cannot drift apart.
//!
//! These are decisions only. They return a descriptor that names an icon; the
//! System resolves and draws it. Nothing here reads a resource fork.
use mac_toolbox::{System, TypeCode,
                  files::{CatalogInfo, Cnid, FinderFlags, ROOT_ID, VolumeId, VolumeInfo},
                  quickdraw::icons::standard};

/// The icon a file list item should be drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon
{
    /// A System icon family, by resource id.
    Resource(i16),
    /// The file's own icon: its custom icon or its bundle's. `fallback` is the
    /// family to draw if the file has none or cannot be read.
    Application
    {
        volume:   VolumeId,
        cnid:     Cnid,
        fallback: i16
    },
    /// A document. The System resolves its icon: its custom icon if it has one,
    /// otherwise the one its creator's application declares for its type. When
    /// the System has neither, draw the icon registered for `file_type`, then
    /// `fallback`.
    Document
    {
        volume:    VolumeId,
        cnid:      Cnid,
        file_type: TypeCode,
        fallback:  i16
    },
    /// The icon registered for this file type. `fallback` is the family to draw
    /// if nothing is registered.
    Associated
    {
        file_type: TypeCode,
        fallback:  i16
    }
}
impl From<i16> for Icon
{
    fn from(id: i16) -> Self
    {
        Self::Resource(id)
    }
}

/// Whether a file list shows this entry. Invisible items are hidden, and so
/// are the two folders at a volume's root that the desktop presents itself.
pub fn visible(entry: &CatalogInfo) -> bool
{
    !entry.finder_flags().contains(FinderFlags::INVISIBLE)
    && !(entry.parent == ROOT_ID
         && entry.is_directory()
         && ["Trash", "Desktop Folder"].contains(&entry.name()))
}

/// The icon family for a volume: the one its driver named, or the floppy.
pub fn volume_icon(volume: &VolumeInfo) -> i16
{
    if volume.icon_kind == TypeCode::new(*b"ICN#") { volume.icon_id } else { standard::FLOPPY }
}

/// The icon family for a folder. The blessed folder and the System's own
/// folders inside it have their own; every other folder is the plain one.
pub fn folder_icon(volume: &VolumeInfo, entry: &CatalogInfo) -> i16
{
    let blessed = volume.blessed_folder;
    if entry.id == blessed
    {
        return standard::SYSTEM_FOLDER;
    }
    if entry.parent == blessed
    {
        // The System's own folders are recognised by name; the table lives with
        // the constants it returns so every caller shares it.
        if let Some(id) = standard::for_blessed_folder(entry.name())
        {
            return id;
        }
    }
    standard::FOLDER
}

/// The icon for any catalogue entry on `volume`.
pub fn entry_icon(system: &System<'_>, volume: &VolumeInfo, entry: &CatalogInfo) -> Icon
{
    if entry.id == ROOT_ID
    {
        return volume_icon(volume).into();
    }
    if entry.is_directory()
    {
        return folder_icon(volume, entry).into();
    }
    // Original System generic stationery family: stacked paper with the
    // lower-right fold. App-specific document/pad compositing is separate.
    if entry.is_stationery()
    {
        return (-3985).into();
    }
    let fallback = system.quickdraw()
                         .icons()
                         .for_file(entry.creator, entry.file_type)
                         .unwrap_or(if entry.file_type == TypeCode::new(*b"APPL")
                                       || entry.file_type == TypeCode::new(*b"FNDR")
                                    {
                                        standard::APPLICATION
                                    }
                                    else
                                    {
                                        standard::DOCUMENT
                                    });
    // Extensions arrive self-decorated: like applications, an INIT file's
    // own BNDL/FREF icon family is its icon, with the generic extension
    // puzzle as fallback. The associated-type path below never opens the
    // file's own fork, so without this every extension shows the generic.
    if entry.file_type == TypeCode::new(*b"APPL") || entry.file_type == TypeCode::new(*b"INIT")
    {
        Icon::Application { volume: volume.id,
                            cnid: entry.id,
                            fallback }
    }
    else
    {
        Icon::Document { volume: volume.id,
                         cnid: entry.id,
                         file_type: entry.file_type,
                         fallback }
    }
}
