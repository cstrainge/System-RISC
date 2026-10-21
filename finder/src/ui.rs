//! Finder text belongs to its resource fork, decoded by the Resource Manager.
use mac_toolbox::{Error, WindowId, *};
pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8]) -> Result<&'b str, Error>
{
    system.resources().application().indexed_string(2000, index, output)
}

#[derive(Default)]
pub struct Alerts<'a>
{
    alert: Option<dialogs::Alert<'a>>,
    empty: bool
}
impl<'a> Alerts<'a>
{
    pub fn window(&self) -> WindowId
    {
        self.alert.as_ref().map_or(WindowId(0), |alert| alert.window())
    }

    pub fn error(&mut self, system: &System<'a>, status: Error) -> Result<(), Error>
    {
        let index = match status
        {
            mac_toolbox::Error::ReadOnly => 11,
            mac_toolbox::Error::AlreadyExists => 12,
            mac_toolbox::Error::AccessDenied => 13,
            mac_toolbox::Error::Io | mac_toolbox::Error::RollbackFailed => 15,
            mac_toolbox::Error::DiskFull => 16,
            mac_toolbox::Error::ServiceBusy => 17,
            _ => 14
        };
        let mut bytes = [0; 768];
        let body = text(system, index, &mut bytes)?;
        self.alert = Some(system.dialogs().alert_owned(resources::APPLICATION, 2001, body)?);
        self.empty = false;
        self.modal_menus(system, false)?;
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
                body.write_str(crate::browser::disk_size_text(usage.allocated_bytes).as_str())
                    .map_err(|_| Error::BadArgument)?;
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
        for id in [1251, 1252, 1253, 1254, 1256, 1255]
        {
            system.menus().set_item_enabled(MenuId(id), 0, enabled)?;
        }
        Ok(())
    }

    /// A true result authorizes the pending Empty Trash action, exactly once.
    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Error>
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
