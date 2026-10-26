//! Each opened resource fork owns its own browser and document window.
use core::fmt::Write;

use spinning_top::{Spinlock, SpinlockGuard};

use super::*;
const MAX_DOCUMENTS: usize = 8;
// Relocated private BSS, not eight large browser/index values on the UI stack.
// Admission never spins; the session owns cleanup before releasing this guard.
static DOCUMENTS: Spinlock<[Option<Document<'static>>; MAX_DOCUMENTS]> =
    Spinlock::new([const { None }; MAX_DOCUMENTS]);
pub(super) struct Document<'a>
{
    // Controls/resources must be disposed before their parent window.
    pub browser: Browser<'a>,
    pub owner:   windows::Window<'a>
}
impl<'a> Document<'a>
{
    #[inline(never)]
    fn new(system: &System<'a>, item: Option<CatalogInfo>) -> ToolboxResult<Self>
    {
        let (bounds, procedure) = window_layout(system)?;
        let mut title = [0; 128];
        let title = system.resources().application().string(128, &mut title)?;
        let owner = system.windows()
                          .create_owned(windows::WindowSpec::new(bounds, title).procedure(procedure)
                                                                               .minimum(600, 400))?;
        let mut browser = Browser::new(system, owner.id())?;
        if let Some(item) = item
        {
            browser.open(system, owner.id(), item)?;
        }
        browser.draw(system, owner.id())?;
        Ok(Self { browser,
                  owner })
    }
}
pub(super) struct Application<'a>
{
    documents:         SpinlockGuard<'a, [Option<Document<'a>>; MAX_DOCUMENTS]>,
    alert:             Option<dialogs::Alert<'a>>,
    documents_pending: bool
}
impl Application<'static>
{
    pub fn acquire() -> ToolboxResult<Self>
    {
        Ok(Self { documents:         DOCUMENTS.try_lock().ok_or(Error::ServiceBusy)?,
                  alert:             None,
                  documents_pending: true })
    }
}
impl Drop for Application<'_>
{
    fn drop(&mut self)
    {
        self.alert = None;
        for document in self.documents.iter_mut()
        {
            *document = None;
        }
    }
}
impl<'a> Application<'a>
{
    // Construct one document at a time in the session's private storage.
    #[inline(never)]
    pub fn initialize(&mut self, system: &System<'a>) -> ToolboxResult<()>
    {
        self.receive(system)?;
        if self.documents.iter().all(Option::is_none) && self.alert.is_none()
        {
            self.documents[0] = Some(Document::new(system, None)?);
        }
        Ok(())
    }

    fn error(&mut self, system: &System<'a>, name: &str, error: Error) -> ToolboxResult<()>
    {
        let mut body = Text::<768>::new();
        write!(body, "{name}\r{error}").map_err(|_| Error::NoMemory)?;
        system.log(body.as_str())?;
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?);
        system.menus().set_item_enabled(MenuId(128), 0, false)
    }

    fn receive(&mut self, system: &System<'a>) -> ToolboxResult<()>
    {
        self.documents_pending = false;
        while let Some(batch) = system.processes().next_document_batch()?
        {
            // A welcome window contains no user document and consumes no slot
            // when a document set arrives. Existing documents remain owned.
            for slot in self.documents.iter_mut()
            {
                if slot.as_ref().is_some_and(|doc| doc.browser.catalog.is_none())
                {
                    *slot = None;
                }
            }
            let mut created = 0u8;
            let mut current = 0;
            let result = (|| {
                for at in 0..batch.len()
                {
                    current = at;
                    let snapshot = batch.info(at)?;
                    if let Some(document)=self.documents.iter().flatten().find(|doc|doc.browser.catalog.is_some_and(|entry|entry.volume == snapshot.volume && entry.id == snapshot.id)) {
                        system.select_window(document.owner.id())?;continue;
                    }
                    let item = system.files().catalog(snapshot.volume, snapshot.id)?;
                    let slot =
                        self.documents.iter().position(Option::is_none).ok_or(Error::NoMemory)?;
                    self.documents[slot] = Some(Document::new(system, Some(item))?);
                    created |= 1 << slot;
                }
                Ok(())
            })();
            if let Err(error) = result
            {
                for (slot, document) in self.documents.iter_mut().enumerate()
                {
                    if created & (1 << slot) != 0
                    {
                        *document = None;
                    }
                }
                let name = batch.info(current).ok();
                self.error(system, name.as_ref().map_or("Res Edit", |item| item.name()), error)?;
            }
            batch.finish()?;
            if self.alert.is_some()
            {
                self.documents_pending = true;
                break;
            }
        }
        Ok(())
    }

    #[inline(never)]
    pub fn event(&mut self, system: &System<'a>, event: &Event<'a>, position: PointerPosition)
                 -> ToolboxResult<bool>
    {
        if matches!(event, Event::DocumentsAvailable { .. })
        {
            self.documents_pending = true;
        }
        if let Some(alert) = &mut self.alert
        {
            if alert.handle_event(event)?.is_some()
            {
                alert.close()?;
                self.alert = None;
                system.menus().set_item_enabled(MenuId(128), 0, true)?;
                if self.documents_pending
                {
                    self.receive(system)?;
                }
            }
            return Ok(true);
        }
        if self.documents_pending
        {
            self.receive(system)?;
            if self.alert.is_some() || matches!(event, Event::DocumentsAvailable { .. })
            {
                return Ok(true);
            }
        }
        let command =
            system.menus().selection(event)?.map(|selection| selection.command.to_be_bytes());
        if command == Some(*b"quit")
        {
            return Ok(false);
        }
        let front =
            event.window().or_else(|| system.window(WindowId(0)).ok().map(|window| window.id));
        let mut slot =
            self.documents
                .iter()
                .position(|doc| doc.as_ref().is_some_and(|doc| Some(doc.owner.id()) == front));
        if command == Some(*b"clos") || matches!(event, Event::CloseRequested { .. })
        {
            if let Some(slot) = slot
            {
                self.documents[slot] = None;
            }
            return Ok(true);
        }
        if command == Some(*b"open") && slot.is_none()
        {
            let free = self.documents.iter().position(Option::is_none).ok_or(Error::NoMemory)?;
            self.documents[free] = Some(Document::new(system, None)?);
            slot = Some(free);
        }
        if let Some(slot) = slot
        {
            let result = super::document_event(system,
                                               self.documents[slot].as_mut().unwrap(),
                                               event,
                                               position,
                                               command);
            if let Err(error) = result
            {
                let name = self.documents[slot].as_ref()
                                               .and_then(|doc| doc.browser.catalog)
                                               .map(|item| item.name);
                self.error(system, name.as_ref().map_or("Res Edit", |name| name.as_str()), error)?;
            }
        }
        Ok(true)
    }
}
