//! File-list icons come from the shared System-backed icon facility.
use std::{cell::RefCell, io, path::Path, rc::Rc};

use mac_toolbox::System;
use sr_files_list::icons::{IconCache, IconRaster};

use crate::native_fs::Metadata;

pub trait IconProvider
{
    fn icon_for(&self, path: &Path, md: &Metadata) -> io::Result<Rc<IconRaster>>;
}
#[derive(Default)]
pub struct SharedIconProvider
{
    cache: RefCell<Option<IconCache<'static>>>
}
impl IconProvider for SharedIconProvider
{
    fn icon_for(&self, _path: &Path, md: &Metadata) -> io::Result<Rc<IconRaster>>
    {
        let system = System::new().map_err(io::Error::other)?;
        let volume = system.files().volume(md.catalog().volume).map_err(io::Error::other)?;
        let mut cache = self.cache.borrow_mut();
        cache.get_or_insert_with(|| IconCache::new(system))
             .resolve_icon_for_file(&volume, md.catalog())
             .map_err(io::Error::other)
    }
}

pub fn geometry() -> io::Result<(usize, usize, usize)>
{
    use mac_toolbox::abi::{pipes, processes};
    let system = System::new().map_err(io::Error::other)?;
    let mut streams = processes::ProcessCall { operation: processes::STANDARD_STREAMS,
                                               ..Default::default() };
    system.client().process(&mut streams).map_err(io::Error::other)?;
    let mut query = pipes::Call { operation: pipes::TERMINAL_QUERY,
                                  handle: streams.streams[1],
                                  ..Default::default() };
    system.client().pipes(&mut query).map_err(io::Error::other)?;
    let t = query.terminal;
    if t.columns == 0 || t.rows == 0
    {
        return Err(io::Error::other("terminal has no cell geometry"));
    }
    let width = usize::from(t.width / t.columns).max(1);
    let height = usize::from(t.height / t.rows).max(1);
    Ok((16usize.div_ceil(width), 16usize.div_ceil(height), usize::from(t.columns)))
}
