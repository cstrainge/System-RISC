//! Finder text belongs to its resource fork, decoded by the Resource Manager.
use core::fmt::Write;

use mac_toolbox::{Error, WindowId, *};
pub fn error_text(system: &System<'_>, status: Error, operation: u32, subject: &str)
                  -> Result<crate::browser::Text<768>, Error>
{
    let requirement = match status
    {
        Error::ServiceRequirement(failure) => Some(failure),
        _ => None
    };
    let index = match requirement.map_or(status, |failure| failure.cause.error())
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
    if let Some(failure) = requirement
    {
        let mut template = [0; 256];
        let template = system.resources().application().indexed_string(20021, 38, &mut template)?;
        let (before, after) = template.split_once("^0").ok_or(Error::BadAbi)?;
        let (middle, after) = after.split_once("^1").ok_or(Error::BadAbi)?;
        write!(&mut out, "{before}{}{middle}{}{after}\r", failure.class, failure.version)
            .map_err(|_| Error::BadArgument)?;
    }
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
    Ok(out)
}
pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8]) -> Result<&'b str, Error>
{
    system.resources().application().indexed_string(2000, index, output)
}

const MODAL_MENUS: [MenuId; 5] =
    [MenuId(1252), MenuId(1253), MenuId(1254), MenuId(1256), MenuId(1255)];
#[derive(Clone, Copy)]
pub(super) enum Completion
{
    Dismissed,
    EmptyTrash,
    RetryFolder(crate::browser::FailedFolder)
}
enum Notice
{
    Error
    {
        cause: Error, operation: u32, subject: crate::browser::Text<256>
    },
    Text
    {
        resource: i16, body: crate::browser::Text<1024>, oom: bool
    }
}
enum State<'a>
{
    Pending
    {
        notice: Notice, completion: Completion, reported: bool
    },
    Open
    {
        alert:      dialogs::Alert<'a>,
        completion: Completion,
        item:       Option<u16>,
        menu:       usize
    },
    Restoring
    {
        completion: Completion, menu: usize
    }
}
#[derive(Default)]
pub struct Alerts<'a>
{
    state:           Option<State<'a>>,
    context:         Option<Event<'a>>,
    retry_at:        Option<u32>,
    retry_needed:    bool,
    menus_installed: bool
}
impl<'a> Alerts<'a>
{
    fn allocation_error(cause: Error) -> bool
    {
        let cause = match cause
        {
            Error::ServiceRequirement(failure) => failure.cause.error(),
            cause => cause
        };
        matches!(cause, Error::NoMemory | Error::MemoryFull)
    }

    pub fn active(&self) -> bool
    {
        self.state.is_some()
    }

    pub fn menus_installed(&mut self)
    {
        self.menus_installed = true;
    }

    fn service_pending(&self) -> bool
    {
        self.retry_needed
        || self.retry_at.is_some()
        || match self.state.as_ref()
        {
            Some(State::Pending { .. } | State::Restoring { .. }) => true,
            Some(State::Open { item,
                               menu,
                               .. }) => item.is_some() || *menu < MODAL_MENUS.len(),
            None => false
        }
    }

    pub fn event_mask(&self) -> events::EventMask
    {
        if self.service_pending()
        {
            // Keep damage in the actual native queue during refused service.
            // Other lifecycle/input remains deliverable; the timeout still runs.
            events::EventMask::from_bits_retain(events::EventMask::ALL.bits()
                                                & !events::EventMask::UPDATE.bits())
        }
        else
        {
            events::EventMask::ALL
        }
    }

    pub fn defer_service(&mut self, system: &System<'_>)
    {
        if !self.retry_needed
        {
            self.defer(system);
        }
    }

    fn defer(&mut self, system: &System<'_>)
    {
        self.retry_needed = true;
        // Clock refusal retains debt without inventing a deadline. Progress
        // must observe the native clock again before attempting service work.
        self.retry_at = match system.events().ticks()
        {
            Ok(now) => Some(now.wrapping_add(30)),
            Err(_) => None
        };
    }

    fn queue(&mut self, system: &System<'a>, notice: Notice, completion: Completion)
             -> Result<(), Error>
    {
        if self.active()
        {
            return Err(Error::ServiceBusy);
        }
        self.state = Some(State::Pending { notice,
                                           completion,
                                           reported: false });
        let result = self.open_pending(system);
        if result.is_err()
        {
            self.defer(system);
        }
        result
    }

    fn open_pending(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        let Some(State::Pending { notice,
                                  completion,
                                  reported }) = self.state.as_mut()
        else
        {
            return Ok(());
        };
        let alert = match notice
        {
            Notice::Error { cause,
                            operation,
                            subject } =>
            {
                let body = error_text(system, *cause, *operation, subject.as_str())?;
                if !*reported
                {
                    *reported = true;
                    let mut log = crate::browser::Text::<1024>::default();
                    let _ = writeln!(&mut log,
                                     "FINDER_ALERT_ERROR status={} error={cause:?}",
                                     cause.status());
                    let _ = writeln!(&mut log, "FINDER_ALERT_TEXT {:?}", body.as_str());
                    let _ = system.log(log.as_str());
                }
                if Self::allocation_error(*cause)
                {
                    system.dialogs().out_of_memory_owned(body.as_str())?
                }
                else
                {
                    system.dialogs().alert_owned(resources::APPLICATION, 2001, body.as_str())?
                }
            }
            Notice::Text { resource,
                           body,
                           oom } =>
            {
                if *oom
                {
                    system.dialogs().out_of_memory_owned(body.as_str())?
                }
                else
                {
                    system.dialogs()
                          .alert_owned(resources::APPLICATION, *resource, body.as_str())?
                }
            }
        };
        let completion = *completion;
        self.state = Some(State::Open { alert,
                                        completion,
                                        item: None,
                                        menu: 0 });
        self.menus(system)
    }

    fn menus(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let (menu, enabled) = match self.state.as_mut()
        {
            Some(State::Open { menu,
                               .. }) => (menu, false),
            Some(State::Restoring { menu,
                                    .. }) => (menu, true),
            _ => return Ok(())
        };
        // A refused startup install has already dropped its partial owners.
        // There is no Menu object to disable/restore before full admission.
        if !self.menus_installed
        {
            *menu = MODAL_MENUS.len();
            return Ok(());
        }
        while *menu < MODAL_MENUS.len()
        {
            system.menus().set_item_enabled(MODAL_MENUS[*menu], 0, enabled)?;
            *menu += 1;
        }
        Ok(())
    }

    pub fn error(&mut self, system: &System<'a>, status: Error) -> Result<(), Error>
    {
        if self.active()
        {
            return Err(Error::ServiceBusy);
        }
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
        self.error_for(system, status, op, subject.as_str())
    }

    pub fn item_open_error(&mut self, system: &System<'a>, item: files::CatalogInfo, cause: Error)
                           -> Result<(), Error>
    {
        let mut log = crate::browser::Text::<384>::default();
        let _ = writeln!(&mut log,
                         "FINDER_ITEM_OPEN_FAILED volume={} cnid={} status={} error={cause:?}",
                         item.volume,
                         item.id,
                         cause.status());
        system.log(log.as_str())?;
        self.error_for(system, cause, 2, item.name())
    }

    pub fn document_open_error(&mut self, system: &System<'a>,
                               failure: crate::associations::OpenFailure)
                               -> Result<(), Error>
    {
        let mut log = crate::browser::Text::<384>::default();
        let _ = writeln!(&mut log,
                         "FINDER_DOCUMENT_OPEN_FAILED volume={} cnid={} status={} error={:?}",
                         failure.document.volume,
                         failure.document.id,
                         failure.cause.status(),
                         failure.cause);
        let _ = system.log(log.as_str());
        self.error_for(system, failure.cause, 2, failure.document.name())
    }

    pub fn open_error(&mut self, system: &System<'a>, status: Error, subject: &str)
                      -> Result<(), Error>
    {
        self.error_for(system, status, 2, subject)
    }

    fn error_for(&mut self, system: &System<'a>, status: Error, operation: u32, subject: &str)
                 -> Result<(), Error>
    {
        self.error_notice(system, status, operation, subject, Completion::Dismissed)
    }

    fn error_notice(&mut self, system: &System<'a>, cause: Error, operation: u32, subject: &str,
                    completion: Completion)
                    -> Result<(), Error>
    {
        let mut owned = crate::browser::Text::<256>::default();
        let mut end = subject.len().min(192);
        while !subject.is_char_boundary(end)
        {
            end -= 1;
        }
        owned.write_str(&subject[..end]).map_err(|_| Error::BadArgument)?;
        if end < subject.len()
        {
            owned.write_str("…").map_err(|_| Error::BadArgument)?;
        }
        self.queue(system,
                   Notice::Error { cause,
                                   operation,
                                   subject: owned },
                   completion)
    }

    pub fn folder_error(&mut self, system: &System<'a>, cause: Error,
                        failed: crate::browser::FailedFolder)
                        -> Result<(), Error>
    {
        if self.active()
        {
            return Err(Error::ServiceBusy);
        }
        // Presentation may omit a refused title lookup; retry authority remains
        // the exact failed binding, never this text or the front Window.
        let window = system.window(failed.window());
        let title = window.as_ref().map_or("Finder", |window| window.title());
        self.error_notice(system, cause, 1, title, Completion::RetryFolder(failed))
    }

    fn text_notice(&mut self, system: &System<'a>, resource: i16, text: &str, oom: bool,
                   completion: Completion)
                   -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        body.write_str(text).map_err(|_| Error::BadArgument)?;
        self.queue(system,
                   Notice::Text { resource,
                                  body,
                                  oom },
                   completion)
    }

    pub fn transfer_error(&mut self, system: &System<'a>, failure: files::TransferFailure)
                          -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        write!(&mut body, "{failure}").map_err(|_| Error::BadArgument)?;
        let mut log = crate::browser::Text::<512>::default();
        let _ = writeln!(&mut log,
                         "FINDER_TRANSFER_ERROR status={} cause={:?} cleanup={:?}",
                         failure.cause.status(),
                         failure.cause,
                         failure.cleanup);
        let _ = system.log(log.as_str());
        let _ = system.log(body.as_str());
        self.text_notice(system,
                         2001,
                         body.as_str(),
                         Self::allocation_error(failure.cause),
                         Completion::Dismissed)
    }

    pub fn power_error(&mut self, system: &System<'a>, failure: mac_toolbox::power::Error)
                       -> Result<(), Error>
    {
        let mut body = crate::browser::Text::<1024>::default();
        write!(&mut body, "{failure}").map_err(|_| Error::BadArgument)?;
        let _ = system.log(body.as_str());
        self.text_notice(system,
                         2001,
                         body.as_str(),
                         Self::allocation_error(failure.cause),
                         Completion::Dismissed)
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
        let _ = system.log(body.as_str());
        self.text_notice(system,
                         2001,
                         body.as_str(),
                         Self::allocation_error(recovery.error),
                         Completion::Dismissed)
    }

    pub fn set_context(&mut self, event: &Event<'a>) -> Result<(), Error>
    {
        if !self.active()
        {
            // Control notifications are handled as typed notifications, not input
            // context for a subsequent alert. They have no raw EventRecord.
            if !matches!(event, Event::Control(_))
            {
                self.context = Some(Event::from(event.as_record()?));
            }
        }
        Ok(())
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
        let mut log = crate::browser::Text::<800>::default();
        let _ = writeln!(&mut log, "FINDER_EMPTY_TEXT {}", body.as_str());
        let _ = system.log(log.as_str());
        self.text_notice(system, 6012, body.as_str(), false, Completion::EmptyTrash)
    }

    /// Genuine item selection, close and menu restoration retain one completion.
    /// Refused service work uses a thirty-native-tick deadline.
    pub fn event(&mut self, system: &System<'a>, browser: &mut crate::browser::Browser,
                 event: &Event)
                 -> Result<Option<Completion>, Error>
    {
        let service = self.service_pending();
        // Consult the clock only for real debt. Queued event timestamps may be
        // old; neither those timestamps nor input intent can grant a retry.
        let now = if service { system.events().ticks()? } else { event.input().when };
        let retry = if service
        {
            match self.retry_at
            {
                Some(at) => now.wrapping_sub(at) < 0x8000_0000,
                None =>
                {
                    self.retry_at = Some(now.wrapping_add(30));
                    false
                }
            }
        }
        else
        {
            false
        };
        if retry
        {
            self.retry_at = None;
            self.retry_needed = false;
        }
        let result = (|| {
            if retry
            {
                browser.finish_modal_input(system)?;
            }
            self.event_inner(system, event, retry)
        })();
        if result.is_err()
        {
            self.defer(system);
        }
        result
    }

    fn event_inner(&mut self, system: &System<'a>, event: &Event, retry: bool)
                   -> Result<Option<Completion>, Error>
    {
        if matches!(self.state, Some(State::Pending { .. }))
        {
            if retry
            {
                self.open_pending(system)?;
            }
            return Ok(None);
        }
        if self.retry_at.is_some() && !retry
        {
            return Ok(None);
        }
        if let Some(State::Open { alert,
                                  item,
                                  .. }) = self.state.as_mut()
        {
            let mut selected = false;
            if item.is_none()
            {
                // Holding Return across a failed retry is not a fresh decision
                // to acknowledge the next notification.
                if !matches!(event,
                             Event::KeyDown { repeat: true,
                                              .. })
                {
                    *item = alert.handle_event(event)?;
                    selected = item.is_some();
                }
            }
            if item.is_some()
            {
                // Record the actual item before a fallible close. A subsequent
                // Idle retries the close, never invents another selection.
                if !selected && !retry
                {
                    return Ok(None);
                }
                alert.close()?;
            }
            else
            {
                if retry
                {
                    self.menus(system)?;
                }
                return Ok(None);
            }
        }
        if let Some(State::Open { completion,
                                  item: Some(item),
                                  .. }) = self.state.as_ref()
        {
            let completion = match completion
            {
                Completion::EmptyTrash if *item != 1 => Completion::Dismissed,
                completion => *completion
            };
            self.state = Some(State::Restoring { completion,
                                                 menu: 0 });
            self.retry_at = None;
            self.retry_needed = false;
            // First restoration follows the successful close immediately.
            self.menus(system)?;
        }
        else if matches!(self.state, Some(State::Restoring { .. }))
        {
            if !retry
            {
                return Ok(None);
            }
            self.menus(system)?;
        }
        let Some(State::Restoring { completion,
                                    menu }) = self.state.as_ref()
        else
        {
            return Ok(None);
        };
        if *menu != MODAL_MENUS.len()
        {
            return Ok(None);
        }
        let completion = *completion;
        self.state = None;
        self.retry_at = None;
        self.retry_needed = false;
        // Diagnostics follow the actual transition and cannot erase its action.
        let _ = system.log(if matches!(completion, Completion::EmptyTrash)
                           {
                               "FINDER_EMPTY_CONFIRMED\n"
                           }
                           else
                           {
                               "FINDER_ALERT_DISMISSED\n"
                           });
        Ok(Some(completion))
    }
}
