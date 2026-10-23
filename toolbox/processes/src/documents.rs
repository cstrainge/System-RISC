//! Resource-declared, whole-set document delivery with explicit ownership.
use mac_abi::processes::*;
pub use mac_traps::DocumentBatchId;

use super::{Client, Cnid, ProcessInfo, Processes, Result, VolumeId};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentRef
{
    pub volume: VolumeId,
    pub id:     Cnid
}
/// A preparing batch owns no application windows. Drop releases all references;
/// only commit can queue the complete set in its exact target instance.
#[must_use]
pub struct DocumentOpen<'a>
{
    client:     Client<'a>,
    id:         DocumentBatchId,
    type_count: u32
}
#[must_use]
pub struct DocumentBatch<'a>
{
    client: Client<'a>,
    id:     DocumentBatchId,
    count:  u32
}
impl<'a> Processes<'a>
{
    pub fn begin_document_open(self, application: DocumentRef) -> Result<DocumentOpen<'a>>
    {
        self.begin_document_open_impl(application, 0)
    }

    fn begin_document_open_impl(self, application: DocumentRef, drag: u32)
                                -> Result<DocumentOpen<'a>>
    {
        let mut call = DocumentCall { operation: DOCUMENT_BEGIN,
                                      application_volume: application.volume.0,
                                      application_cnid: application.id.0,
                                      drag,
                                      ..Default::default() };
        self.client.document(&mut call)?;
        if call.handle == 0 || call.count == 0 || call.count > 256
        {
            return Err(mac_traps::Error::BadAbi);
        }
        Ok(DocumentOpen { client:     self.client,
                          id:         DocumentBatchId(call.handle),
                          type_count: call.count })
    }

    pub fn open_documents(self, application: DocumentRef, documents: &[DocumentRef])
                          -> Result<ProcessInfo>
    {
        let mut batch = self.begin_document_open(application)?;
        for &document in documents
        {
            batch.push(document)?;
        }
        batch.commit()
    }

    pub fn next_document_batch(self) -> Result<Option<DocumentBatch<'a>>>
    {
        let mut call = DocumentCall { operation: DOCUMENT_NEXT,
                                      ..Default::default() };
        self.client.document(&mut call)?;
        match call.present
        {
            0 if call.handle == 0 && call.count == 0 => Ok(None),
            1 if call.handle != 0 && call.count as usize <= MAX_DOCUMENTS && call.count != 0 =>
            {
                Ok(Some(DocumentBatch { client: self.client,
                                        id:     DocumentBatchId(call.handle),
                                        count:  call.count }))
            }
            _ => Err(mac_traps::Error::BadAbi)
        }
    }
}
impl DocumentOpen<'_>
{
    /// Number of distinct resource-declared types accepted by this application.
    pub fn supported_type_count(&self) -> u32
    {
        self.type_count
    }

    pub fn id(&self) -> DocumentBatchId
    {
        self.id
    }

    pub fn accepts(&self, file_type: mac_traps::TypeCode) -> Result<bool>
    {
        let mut call = DocumentCall { operation: DOCUMENT_ACCEPTS,
                                      handle: self.id.0,
                                      file_type: file_type.bytes(),
                                      ..Default::default() };
        self.client.document(&mut call)?;
        match call.present
        {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(mac_traps::Error::BadAbi)
        }
    }

    pub fn push(&mut self, document: DocumentRef) -> Result<()>
    {
        let mut call = DocumentCall { operation: DOCUMENT_PUSH,
                                      handle: self.id.0,
                                      volume: document.volume.0,
                                      cnid: document.id.0,
                                      ..Default::default() };
        self.client.document(&mut call)
    }

    pub fn commit(mut self) -> Result<ProcessInfo>
    {
        let mut call = DocumentCall { operation: DOCUMENT_COMMIT,
                                      handle: self.id.0,
                                      ..Default::default() };
        self.client.document(&mut call)?;
        let process = call.process.try_into()?;
        self.id = DocumentBatchId(0);
        Ok(process)
    }
}
impl Drop for DocumentOpen<'_>
{
    fn drop(&mut self)
    {
        if self.id.0 != 0
        {
            let _ = self.client.document(&mut DocumentCall { operation: DOCUMENT_ABORT,
                                                             handle: self.id.0,
                                                             ..Default::default() });
        }
    }
}
impl DocumentBatch<'_>
{
    pub fn id(&self) -> DocumentBatchId
    {
        self.id
    }

    pub fn len(&self) -> u32
    {
        self.count
    }

    pub fn is_empty(&self) -> bool
    {
        self.count == 0
    }

    /// The validated delivery snapshot; opening its file still checks current
    /// mounted-volume generation and catalog identity through Files.
    pub fn info(&self, index: u32) -> Result<toolbox_files::CatalogInfo>
    {
        let mut call = DocumentCall { operation: DOCUMENT_AT,
                                      handle: self.id.0,
                                      index,
                                      ..Default::default() };
        self.client.document(&mut call)?;
        call.entry.try_into()
    }

    pub fn at(&self, index: u32) -> Result<DocumentRef>
    {
        let entry = self.info(index)?;
        Ok(DocumentRef { volume: entry.volume,
                         id:     entry.id })
    }

    pub fn finish(mut self) -> Result<()>
    {
        self.client.document(&mut DocumentCall { operation: DOCUMENT_FINISH,
                                                  handle: self.id.0,
                                                  ..Default::default() })?;
        self.id = DocumentBatchId(0);
        Ok(())
    }
}
impl Drop for DocumentBatch<'_>
{
    fn drop(&mut self)
    {
        if self.id.0 != 0
        {
            let _ = self.client.document(&mut DocumentCall { operation: DOCUMENT_FINISH,
                                                             handle: self.id.0,
                                                             ..Default::default() });
        }
    }
}
