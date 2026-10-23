//! Lazy application discovery. Directory watches invalidate the retained index;
//! one scan or declaration probe runs per Finder turn, never per pointer event.
use mac_toolbox::{files::{CatalogInfo, DirectoryCursor, DirectoryWatch, ROOT_ID, ScanStep},
                  processes::{DocumentOpen, DocumentRef},
                  *};

const MAX_APPLICATIONS: usize = 64;
#[derive(Clone, Copy)]
struct Application
{
    file:    DocumentRef,
    creator: TypeCode
}
struct Directory<'a>
{
    file:   DocumentRef,
    watch:  DirectoryWatch<'a>,
    cursor: DirectoryCursor,
    done:   bool
}
struct Request<'a>
{
    document:   CatalogInfo,
    probe:      usize,
    best:       Option<DocumentOpen<'a>>,
    rank:       (bool, u32),
    diagnostic: Option<Error>
}
pub struct Controller<'a>
{
    directories:  [Option<Directory<'a>>; 2],
    applications: [Option<Application>; MAX_APPLICATIONS],
    count:        usize,
    request:      Option<Request<'a>>
}
impl Default for Controller<'_>
{
    fn default() -> Self
    {
        Self { directories:  core::array::from_fn(|_| None),
               applications: [None; MAX_APPLICATIONS],
               count:        0,
               request:      None }
    }
}
impl<'a> Controller<'a>
{
    pub fn pending(&self) -> bool
    {
        self.request.is_some()
    }

    fn add(&mut self, application: Application) -> Result<(), Error>
    {
        if self.applications[..self.count].iter().flatten().any(|old| old.file == application.file)
        {
            return Ok(());
        }
        let slot = self.applications.get_mut(self.count).ok_or(Error::NoMemory)?;
        *slot = Some(application);
        self.count += 1;
        Ok(())
    }

    pub fn begin(&mut self, system: &System<'a>, startup: VolumeId, document: CatalogInfo)
                 -> Result<(), Error>
    {
        if self.pending()
        {
            return Err(Error::ServiceBusy);
        }
        let source = DocumentRef { volume: document.volume,
                                   id:     document.parent };
        let installed = match system.files().lookup(startup, ROOT_ID, "Applications")
        {
            Ok(folder) =>
            {
                Some(DocumentRef { volume: startup,
                                   id:     folder.id })
            }
            Err(Error::NotFound) => None,
            Err(error) => return Err(error)
        };
        let wanted = [Some(source), installed.filter(|folder| *folder != source)];
        let mut stale = false;
        for (slot, wanted) in self.directories.iter_mut().zip(wanted)
        {
            if slot.as_ref().map(|directory| directory.file) != wanted
            {
                *slot = match wanted
                {
                    Some(file) =>
                    {
                        Some(Directory { file,
                                         watch: system.files()
                                                      .watch_directory(file.volume, file.id)?,
                                         cursor: DirectoryCursor::default(),
                                         done: false })
                    }
                    None => None
                };
                stale = true;
            }
            else if let Some(directory) = slot
            {
                stale |= directory.watch.poll()?.is_some();
            }
        }
        if stale
        {
            self.applications.fill(None);
            self.count = 0;
            for directory in self.directories.iter_mut().flatten()
            {
                directory.cursor = DirectoryCursor::default();
                directory.done = false;
            }
        }
        // Live applications may have been launched from any mounted directory.
        for at in 0..system.processes().count()?
        {
            let process = system.processes().at(at)?;
            if process.cnid.0 <= ROOT_ID.0
            {
                continue;
            }
            let entry = system.files().catalog(process.volume, process.cnid)?;
            self.add(Application { file:    DocumentRef { volume: entry.volume,
                                                          id:     entry.id },
                                   creator: entry.creator })?;
        }
        self.request = Some(Request { document,
                                      probe: 0,
                                      best: None,
                                      rank: (true, u32::MAX),
                                      diagnostic: None });
        Ok(())
    }

    pub fn advance(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        let Some(mut request) = self.request.take()
        else
        {
            return Ok(());
        };
        if let Some(directory) =
            self.directories.iter_mut().flatten().find(|directory| !directory.done)
        {
            let scan =
                system.files().scan_directory(directory.file.volume, directory.file.id,
                                              &mut directory.cursor);
            let scan = match scan
            {
                Ok(scan) => scan,
                Err(error) =>
                {
                    self.applications.fill(None);
                    self.count = 0;
                    for directory in self.directories.iter_mut().flatten()
                    {
                        directory.cursor = DirectoryCursor::default();
                        directory.done = false;
                    }
                    return Err(error);
                }
            };
            match scan
            {
                ScanStep::Entry { catalog,
                                  .. } if catalog.file_type == TypeCode::new(*b"APPL") && !catalog.is_directory() =>
                {
                    self.add(Application { file:    DocumentRef { volume: catalog.volume,
                                                                  id:     catalog.id },
                                           creator: catalog.creator })?;
                }
                ScanStep::Done => directory.done = true,
                _ => ()
            }
        }
        else if request.probe < self.count
        {
            let application = self.applications[request.probe].ok_or(Error::BadAbi)?;
            request.probe += 1;
            // Creator match wins. Otherwise prefer the most specific declared
            // handler, so a general resource browser does not steal TEXT files.
            match system.processes().begin_document_open(application.file)
            {
                Ok(batch) =>
                {
                    match batch.accepts(request.document.file_type)
                    {
                        Ok(true) =>
                        {
                            let rank = (application.creator != request.document.creator,
                                        batch.supported_type_count());
                            if rank < request.rank
                            {
                                request.rank = rank;
                                request.best = Some(batch);
                            }
                        }
                        Ok(false) => (),
                        Err(error) => request.diagnostic = Some(error)
                    }
                }
                Err(Error::NotSupported | Error::NotFound) => (),
                Err(error) => request.diagnostic = Some(error)
            }
        }
        else
        {
            let mut batch =
                request.best.ok_or(request.diagnostic.unwrap_or(Error::NotSupported))?;
            batch.push(DocumentRef { volume: request.document.volume,
                                     id:     request.document.id })?;
            batch.commit()?;
            return Ok(());
        }
        self.request = Some(request);
        Ok(())
    }
}
