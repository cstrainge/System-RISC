//! Finder text belongs to its resource fork, decoded by the Resource Manager.
use mac_toolbox::*;
pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8]) -> Result<&'b str, Status>
{
    system.resources().application().indexed_string(2000, index, output).map_err(|e| e.0)
}

#[derive(Default)]
pub struct Alerts
{
    pub window: u32,
    empty:      bool
}
impl Alerts
{
    pub fn error(&mut self, system: &System<'_>, status: Status) -> Result<(), Status>
    {
        let index = match status
        {
            mac_toolbox::READ_ONLY => 11,
            mac_toolbox::ALREADY_EXISTS => 12,
            mac_toolbox::ACCESS_DENIED => 13,
            mac_toolbox::IO_ERROR | mac_toolbox::ROLLBACK_FAILED => 15,
            mac_toolbox::DISK_FULL => 16,
            mac_toolbox::SERVICE_BUSY => 17,
            _ => 14
        };
        let mut bytes = [0; 768];
        let body = text(system, index, &mut bytes)?;
        self.window =
            system.dialogs().alert(resources::APPLICATION, 2001, body).map_err(|e| e.0)?.0;
        self.empty = false;
        self.modal_menus(system, false)?;
        Ok(())
    }

    pub fn confirm_empty(&mut self, system: &System<'_>, browser: &crate::browser::Browser)
                         -> Result<(), Status>
    {
        use core::fmt::Write;
        let usage = browser.trash_usage(system)?;
        let mut bytes = [0; 768];
        let template = system.resources()
                             .application()
                             .indexed_string(11750, 5, &mut bytes)
                             .map_err(|e| e.0)?;
        let mut body = crate::browser::Text::<768>::default();
        let mut remaining = template;
        while !remaining.is_empty()
        {
            if let Some(after) = remaining.strip_prefix("%1%")
            {
                let (single, after) = after.split_once('%').ok_or(BAD_ARGUMENT)?;
                let (plural, after) = after.split_once('%').ok_or(BAD_ARGUMENT)?;
                body.write_str(if usage.items == 1 { single } else { plural })
                    .map_err(|_| BAD_ARGUMENT)?;
                remaining = after;
            }
            else if let Some(after) = remaining.strip_prefix("^1")
            {
                write!(&mut body, "{}", usage.items).map_err(|_| BAD_ARGUMENT)?;
                remaining = after;
            }
            else if let Some(after) = remaining.strip_prefix("^2")
            {
                body.write_str(crate::browser::disk_size_text(usage.allocated_bytes).as_str())
                    .map_err(|_| BAD_ARGUMENT)?;
                remaining = after;
            }
            else
            {
                let ch = remaining.chars().next().ok_or(BAD_ARGUMENT)?;
                if ch == '%' || ch == '^'
                {
                    return Err(NOT_SUPPORTED);
                }
                body.write_char(ch).map_err(|_| BAD_ARGUMENT)?;
                remaining = &remaining[ch.len_utf8()..];
            }
        }
        self.window =
            system.dialogs().alert(resources::APPLICATION, 6012, body.as_str()).map_err(|e| e.0)?.0;
        let mut log = crate::browser::Text::<800>::default();
        let _ = writeln!(&mut log, "FINDER_EMPTY_TEXT {}", body.as_str());
        system.log(log.as_str())?;
        self.empty = true;
        self.modal_menus(system, false)?;
        system.log("FINDER_EMPTY_CONFIRM\n")
    }

    fn modal_menus(&self, system: &System<'_>, enabled: bool) -> Result<(), Status>
    {
        for id in [1251, 1252, 1253, 1254, 1256, 1255, 1257]
        {
            system.menus().set_item_enabled(id, 0, enabled).map_err(|e| e.0)?;
        }
        Ok(())
    }

    /// A true result authorizes the pending Empty Trash action, exactly once.
    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Status>
    {
        let Some(item) =
            system.dialogs().handle_event(WindowId(self.window), event).map_err(|e| e.0)?
        else
        {
            return Ok(false);
        };
        system.dialogs().close(WindowId(self.window)).map_err(|e| e.0)?;
        self.window = 0;
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
