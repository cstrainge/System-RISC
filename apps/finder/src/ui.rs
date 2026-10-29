//! Finder text belongs to its resource fork, decoded by the Resource Manager.
use core::fmt::Write;

use mac_toolbox::{Error, WindowId, *};
pub fn error_text(system: &System<'_>, status: Error, operation: u32, subject: &str)
                  -> Result<crate::browser::Text<768>, Error>
{
    let index = match status
    {
        Error::BadAbi => 0,
        Error::BadArgument => 1,
        Error::NotFound => 2,
        Error::NotSupported => 3,
        Error::NoMemory => 4,
        Error::TranslationRequired => 5,
        Error::ServiceBusy => 6,
        Error::WrongThread => 7,
        Error::Io => 8,
        Error::ReadOnly => 9,
        Error::AlreadyExists => 10,
        Error::AccessDenied => 11,
        Error::DiskFull => 12,
        Error::RollbackFailed => 13,
        Error::DirectoryNotEmpty => 14,
        Error::ScanStale => 15,
        Error::Cancelled => 16,
        Error::EventNotEnabled => 17,
        Error::ScrapTypeUnavailable => 18,
        Error::MemoryFull => 19,
        Error::NilHandle => 20,
        Error::InvalidZone => 21,
        Error::HandleNotPurgeable => 22,
        Error::HandleLocked => 23,
        Error::CorruptData => 24,
        Error::StorageUnavailable => 25,
        Error::InvalidName => 27,
        Error::Archive(error) =>
        {
            match error
            {
                files::ArchiveError::Truncated => 28,
                files::ArchiveError::Signature => 29,
                files::ArchiveError::HeaderChecksum => 30,
                files::ArchiveError::ForkChecksum => 31,
                files::ArchiveError::Malformed => 32,
                files::ArchiveError::Limit => 33,
                files::ArchiveError::Encrypted => 34,
                files::ArchiveError::Version(_) => 35,
                files::ArchiveError::Method(_) => 36,
                files::ArchiveError::EntryVersion(_) => 37
            }
        }
        _ => 26
    };
    let mut a = [0; 256];
    let mut b = [0; 384];
    let operation = system.resources().application().indexed_string(20020, operation, &mut a)?;
    let cause = system.resources().application().indexed_string(20021, index, &mut b)?;
    let mut out = crate::browser::Text::default();
    write!(&mut out, "{operation}: {subject}\r").map_err(|_| Error::BadArgument)?;
    if let Error::Archive(files::ArchiveError::Version(number)
                          | files::ArchiveError::Method(number)
                          | files::ArchiveError::EntryVersion(number)) = status
    {
        let (before, after) = cause.split_once("^0").ok_or(Error::BadAbi)?;
        write!(&mut out, "{before}{number}{after}").map_err(|_| Error::BadArgument)?;
    }
    else
    {
        out.write_str(cause).map_err(|_| Error::BadArgument)?;
    }
    write!(&mut out, "\r{status:?} ({})", status.status()).map_err(|_| Error::BadArgument)?;
    Ok(out)
}
pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8]) -> Result<&'b str, Error>
{
    system.resources().application().indexed_string(2000, index, output)
}

#[derive(Default)]
pub struct Alerts<'a>
{
    alert:   Option<dialogs::Alert<'a>>,
    empty:   bool,
    context: Option<Event<'a>>
}
impl<'a> Alerts<'a>
{
    pub fn window(&self) -> WindowId
    {
        self.alert.as_ref().map_or(WindowId(0), |alert| alert.window())
    }

    pub fn error(&mut self, system: &System<'a>, status: Error) -> Result<(), Error>
    {
        let op = match self.context.as_ref()
        {
            Some(Event::Update { .. }) => 1,
            Some(Event::MouseDown { .. }) => 2,
            Some(Event::MenuCommand { menu: MenuId(1252),
                                      item,
                                      .. }) =>
            {
                match item
                {
                    1 => 3,
                    2 => 2,
                    6 => 6,
                    10 => 10,
                    4 => 9,
                    _ => 8
                }
            }
            Some(Event::MenuCommand { menu: MenuId(1255),
                                      item: 2,
                                      .. }) => 5,
            Some(Event::KeyDown { .. }) =>
            {
                match self.context
                          .as_ref()
                          .unwrap()
                          .key()
                          .map_or('\0', |k| k.character().to_ascii_lowercase())
                {
                    'n' => 3,
                    'o' => 2,
                    'i' => 6,
                    'w' => 9,
                    _ => 8
                }
            }
            None => 0,
            _ => 8
        };
        let mut subject = crate::browser::Text::<256>::default();
        let window = self.context.as_ref().and_then(|e| e.window()).unwrap_or(WindowId(0));
        if let Some(event) = self.context.as_ref().filter(|e| e.window().is_none())
           && let Some(index) = crate::browser::desktop::hit(event.input().at)
           && let Some(item) = crate::browser::desktop::item(index)
        {
            let _ = subject.write_str(item.name.as_str());
        }
        if subject.as_str().is_empty()
        {
            if let Ok(w) = system.window(window)
            {
                let _ = subject.write_str(w.title());
            }
            else
            {
                let _ = subject.write_str("Finder");
            }
        }
        let body = error_text(system, status, op, subject.as_str())?;
        let mut log = crate::browser::Text::<1024>::default();
        let _ = writeln!(&mut log, "FINDER_ALERT_TEXT {:?}", body.as_str());
        system.log(log.as_str())?;
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?);
        self.empty = false;
        self.modal_menus(system, false)?;
        Ok(())
    }

    pub fn transfer_error(&mut self, system: &System<'a>, failure: files::TransferFailure)
                          -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        write!(&mut body,"{failure}\r{:?} ({})",failure.cause,failure.cause.status()).map_err(|_|Error::BadArgument)?;
        system.log(body.as_str())?;
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?);
        self.empty = false;
        self.modal_menus(system, false)
    }

    pub fn power_error(&mut self, system: &System<'a>, failure: mac_toolbox::power::Error)
                       -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        write!(&mut body, "{failure}").map_err(|_| Error::BadArgument)?;
        system.log(body.as_str())?;
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?);
        self.empty = false;
        self.modal_menus(system, false)
    }

    pub fn transfer_recovery(&mut self, system: &System<'a>, recovery: files::TransferRecovery)
                             -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        if let Some(item) = recovery.current
        {
            write!(&mut body, "{}\r", item.name()).map_err(|_| Error::BadArgument)?;
        }
        if let Some(original) = recovery.original
        {
            write!(&mut body, "{original}\r").map_err(|_| Error::BadArgument)?;
        }
        let mut bytes = [0; 256];
        let advice = system.resources().application().string(if recovery.committed
                                                              {
                                                                  2006
                                                              }
                                                              else
                                                              {
                                                                  2005
                                                              },
                                                              &mut bytes)?;
        write!(&mut body, "{}\r{advice}", recovery.error).map_err(|_| Error::BadArgument)?;
        system.log(body.as_str())?;
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?);
        self.empty = false;
        self.modal_menus(system, false)
    }

    pub fn set_context(&mut self, event: &Event<'a>)
    {
        if self.alert.is_none()
        {
            self.context = Some(Event::from(event.as_record()));
        }
    }

    pub fn confirm_empty(&mut self, system: &System<'a>, browser: &crate::browser::Browser)
                         -> Result<(), Error>
    {
        use core::fmt::Write;
        let usage = browser.trash_usage(system)?;
        let mut bytes = [0; 768];
        let template = system.resources().application().indexed_string(11750, 5, &mut bytes)?;
        let mut body = crate::browser::Text::<768>::default();
        let mut remaining = template;
        while !remaining.is_empty()
        {
            if let Some(after) = remaining.strip_prefix("%1%")
            {
                let (single, after) = after.split_once('%').ok_or(Error::BadArgument)?;
                let (plural, after) = after.split_once('%').ok_or(Error::BadArgument)?;
                body.write_str(if usage.items == 1 { single } else { plural })
                    .map_err(|_| Error::BadArgument)?;
                remaining = after;
            }
            else if let Some(after) = remaining.strip_prefix("^1")
            {
                write!(&mut body, "{}", usage.items).map_err(|_| Error::BadArgument)?;
                remaining = after;
            }
            else if let Some(after) = remaining.strip_prefix("^2")
            {
                if let Some(bytes) = usage.allocated_bytes
                {
                    body.write_str(crate::browser::disk_size_text(bytes).as_str())
                        .map_err(|_| Error::BadArgument)?;
                }
                else
                {
                    let mut bytes = [0; 64];
                    let unknown = system.resources().application().string(20022, &mut bytes)?;
                    body.write_str(unknown).map_err(|_| Error::BadArgument)?;
                }
                remaining = after;
            }
            else
            {
                let ch = remaining.chars().next().ok_or(Error::BadArgument)?;
                if ch == '%' || ch == '^'
                {
                    return Err(Error::NotSupported);
                }
                body.write_char(ch).map_err(|_| Error::BadArgument)?;
                remaining = &remaining[ch.len_utf8()..];
            }
        }
        self.alert =
            Some(system.dialogs().alert_owned(resources::APPLICATION, 6012, body.as_str())?);
        let mut log = crate::browser::Text::<800>::default();
        let _ = writeln!(&mut log, "FINDER_EMPTY_TEXT {}", body.as_str());
        system.log(log.as_str())?;
        self.empty = true;
        self.modal_menus(system, false)?;
        system.log("FINDER_EMPTY_CONFIRM\n")
    }

    fn modal_menus(&self, system: &System<'_>, enabled: bool) -> Result<(), Error>
    {
        for id in [1252, 1253, 1254, 1256, 1255]
        {
            system.menus().set_item_enabled(MenuId(id), 0, enabled)?;
        }
        Ok(())
    }

    /// A true result authorizes the pending Empty Trash action, exactly once.
    pub fn event(&mut self, system: &System<'_>, event: &Event) -> Result<bool, Error>
    {
        let Some(alert) = &mut self.alert
        else
        {
            return Ok(false);
        };
        let Some(item) = alert.handle_event(event)?
        else
        {
            return Ok(false);
        };
        alert.close()?;
        self.alert = None;
        self.modal_menus(system, true)?;
        let confirmed = self.empty && item == 1;
        self.empty = false;
        system.log(if confirmed
                   {
                       "FINDER_EMPTY_CONFIRMED\n"
                   }
                   else
                   {
                       "FINDER_ALERT_DISMISSED\n"
                   })?;
        Ok(confirmed)
    }
}
