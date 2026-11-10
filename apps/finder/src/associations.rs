//! Opening a document. The System decides which application opens it; Finder
//! asks, then hands the document over. One step runs per Finder turn, never
//! per pointer event.
use mac_toolbox::{files::CatalogInfo,
                  processes::DocumentRef,
                  *};

struct Request
{
    document: CatalogInfo
}
/// The failed Open owns its original catalog identity and display snapshot.
/// Neither a later turn nor a later selection can change its subject.
pub struct OpenFailure
{
    pub document: CatalogInfo,
    pub cause:    Error
}
#[derive(Default)]
pub struct Controller<'a>
{
    request: Option<Request>,
    system:  core::marker::PhantomData<&'a ()>
}
impl<'a> Controller<'a>
{
    pub fn pending(&self) -> bool
    {
        self.request.is_some()
    }

    pub fn begin(&mut self, _system: &System<'a>, _startup: VolumeId, document: CatalogInfo)
                 -> Result<(), OpenFailure>
    {
        if self.pending()
        {
            return Err(OpenFailure { document,
                                     cause: Error::ServiceBusy });
        }
        self.request = Some(Request { document });
        Ok(())
    }

    pub fn advance(&mut self, system: &System<'a>) -> Result<(), OpenFailure>
    {
        let Some(request) = self.request.take()
        else
        {
            return Ok(());
        };
        let document = request.document;
        let result = (|| {
            let application =
                match system.files().handler_for(document.creator, document.file_type)
                {
                    Ok(application) => application,
                    // The System is still indexing applications. Keep the
                    // request and ask again next turn.
                    Err(Error::ServiceBusy) => return Ok(false),
                    Err(Error::NotFound) => return Err(Error::NotSupported),
                    Err(error) => return Err(error)
                };
            let mut batch =
                system.processes().begin_document_open(DocumentRef { volume: application.volume,
                                                                     id:     application.id })?;
            // The index is a moment old. The application's own declaration is
            // the authority at the instant of opening.
            if !batch.accepts(document.file_type)?
            {
                return Err(Error::NotSupported);
            }
            batch.push(DocumentRef { volume: document.volume,
                                     id:     document.id })?;
            batch.commit()?;
            Ok(true)
        })();
        match result
        {
            Ok(true) => Ok(()),
            Ok(false) =>
            {
                self.request = Some(request);
                Ok(())
            }
            Err(cause) =>
            {
                Err(OpenFailure { document,
                                  cause })
            }
        }
    }
}
