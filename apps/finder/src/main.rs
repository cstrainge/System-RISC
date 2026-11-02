//! Finder is a separate resource-fork application using System Toolbox services.

use mac_toolbox::{System, *};
mac_toolbox::include_resource_ids!();

mod ui;
mod clipboard;
mod associations;
mod menus;
mod browser;
mod cache;
mod selection;
mod watches;
mod icon_view;
mod get_info;
mod font_suitcase;

fn update_menus(system: &System<'_>, browser: &browser::Browser, info: &get_info::Controller)
                -> Result<(), Error>
{
    menus::update(system, browser)?;
    info.update_menus(system)
}

fn try_action<'a, T>(system: &System<'a>, alerts: &mut ui::Alerts<'a>, result: Result<T, Error>)
                     -> Result<Option<T>, Error>
{
    match result
    {
        Ok(value) => Ok(Some(value)),
        Err(status) =>
        {
            action_error(system, alerts, status, None)?;
            Ok(None)
        }
    }
}

fn try_open_action<'a, T>(system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                          result: Result<T, browser::ActionFailure>)
                          -> Result<Option<T>, Error>
{
    use core::fmt::Write;
    match result
    {
        Ok(value) => Ok(Some(value)),
        Err(browser::ActionFailure::Other(cause)) => try_action(system, alerts, Err(cause)),
        Err(browser::ActionFailure::Open { item,
                                           cause }) =>
        {
            if let Err(error) = alerts.item_open_error(system, item, cause)
            {
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
                system.log(message.as_str())?;
            }
            Ok(None)
        }
    }
}

/// Startup debt survives error presentation. Successful stages keep their real
/// objects; only a successful native acknowledgement retires this state.
struct Startup<'a>
{
    menus:        Option<[mac_toolbox::menus::MenuRef<'a>; 5]>,
    initialized:  bool,
    menus_ready:  bool,
    restore_menu: Option<usize>,
    pending:      bool
}
impl<'a> Startup<'a>
{
    fn new() -> Self
    {
        Self { menus:        None,
               initialized:  false,
               menus_ready:  false,
               restore_menu: None,
               pending:      true }
    }

    fn update_menus(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                    browser: &browser::Browser, info: &get_info::Controller)
                    -> Result<(), Error>
    {
        let result = update_menus(system, browser, info);
        if !self.pending
        {
            return result;
        }
        self.menus_ready = try_action(system, alerts, result)?.is_some();
        Ok(())
    }

    fn poll_watches(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                    watches: &mut watches::Controller<'a>, browser: &mut browser::Browser)
                    -> Result<(), Error>
    {
        let result = watches.poll(system, browser);
        let success = try_action(system, alerts, result)?.is_some();
        browser.startup_watches_polled(success);
        Ok(())
    }

    fn update_info_menus(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                         info: &get_info::Controller)
                         -> Result<(), Error>
    {
        let result = info.update_menus(system);
        if !self.pending
        {
            return result;
        }
        self.menus_ready &= try_action(system, alerts, result)?.is_some();
        Ok(())
    }

    fn alert_event(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>, event: &Event<'a>)
                   -> Result<bool, Error>
    {
        match alerts.event(system, event)
        {
            Err(error) if self.pending =>
            {
                // Preserve the existing Alert when event/close refused. If it
                // already closed, retry the failed menu restoration directly;
                // opening another modal Alert would prevent that restoration.
                self.menus_ready = false;
                if alerts.window() == WindowId(0)
                {
                    self.restore_menu = Some(0);
                }
                use core::fmt::Write;
                let mut line = browser::Text::<160>::default();
                let _ = writeln!(&mut line,
                                 "FINDER_ALERT_EVENT_FAILED status={} error={error:?}",
                                 error.status());
                system.log(line.as_str())?;
                Ok(false)
            }
            result => result
        }
    }

    fn advance(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
               browser: &mut browser::Browser, info: &get_info::Controller, settled_turn: bool)
               -> Result<(), Error>
    {
        if !self.pending || alerts.window() != WindowId(0)
        {
            return Ok(());
        }
        if self.menus.is_none()
        {
            self.menus = try_action(system, alerts, menus::install(system))?;
            if self.menus.is_none()
            {
                return Ok(());
            }
        }
        if !self.initialized
        {
            self.initialized = try_action(system, alerts, browser.initialize(system))?.is_some();
            if !self.initialized
            {
                return Ok(());
            }
            self.menus_ready = false;
        }
        while let Some(index) = self.restore_menu
        {
            let Some(menus) = &self.menus
            else
            {
                return Ok(());
            };
            if index == menus.len()
            {
                self.restore_menu = None;
                break;
            }
            if try_action(system, alerts, menus[index].set_item_enabled(0, true))?.is_none()
            {
                return Ok(());
            }
            self.restore_menu = Some(index + 1);
        }
        if !settled_turn
        {
            return Ok(());
        }
        if try_action(system, alerts, browser.retry_startup_content(system))?.is_none()
        {
            return Ok(());
        }
        if try_action(system, alerts, browser.startup_ready(system))? != Some(true)
        {
            return Ok(());
        }
        if !self.menus_ready
        {
            self.update_menus(system, alerts, browser, info)?;
        }
        if !self.menus_ready || alerts.window() != WindowId(0)
        {
            return Ok(());
        }
        if try_action(system, alerts, system.processes().finder_desktop_ready())?.is_some()
        {
            self.pending = false;
            browser.finish_startup_confirmation();
            system.log("DESKTOP_READY: Finder and native Window Manager\n")?;
        }
        Ok(())
    }
}

fn try_document_open<'a>(system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                         result: Result<(), associations::OpenFailure>)
                         -> Result<bool, Error>
{
    use core::fmt::Write;
    match result
    {
        Ok(()) => Ok(true),
        Err(failure) =>
        {
            if let Err(error) = alerts.document_open_error(system, failure)
            {
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
                system.log(message.as_str())?;
            }
            Ok(false)
        }
    }
}

fn action_error<'a>(system: &System<'a>, alerts: &mut ui::Alerts<'a>, status: Error,
                    document: Option<&str>)
                    -> Result<(), Error>
{
    use core::fmt::Write;
    let mut message = browser::Text::<80>::default();
    let _ =
        writeln!(&mut message, "FINDER_ACTION_FAILED status={} error={status:?}", status.status());
    system.log(message.as_str())?;
    let shown = if let Some(subject) = document
    {
        alerts.open_error(system, status, subject)
    }
    else
    {
        alerts.error(system, status)
    };
    if let Err(error) = shown
    {
        let mut message = browser::Text::<80>::default();
        let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
        system.log(message.as_str())?;
    }
    Ok(())
}
fn main() -> Result<(), Error>
{
    let system = &System::new()?;
    system.log("Finder: native application entry reached\n")?;
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return Ok(());
    }
    let mut browser = browser::Browser::default();
    let mut dragging = browser::dragging::Controller::default();
    let mut watches = watches::Controller::default();
    let mut associations = associations::Controller::default();
    let mut info = get_info::Controller::default();
    let mut font_suitcases = font_suitcase::Controller::default();
    let mut alerts = ui::Alerts::default();
    let mut startup = Startup::new();
    browser.begin_startup_confirmation();
    startup.advance(system, &mut alerts, &mut browser, &info, false)?;
    let mut clipboard = clipboard::Viewer::default();
    let mut startup_retry = false;
    let mut deferred_command = None;
    loop
    {
        if startup_retry && !dragging.runnable() && !dragging.positions_pending(&browser)
        {
            startup_retry = false;
            startup.advance(system, &mut alerts, &mut browser, &info, true)?;
        }
        if alerts.window() == WindowId(0)
        {
            let accepted = if let Some(document) = browser.take_document_open()
            {
                let result =
                    if font_suitcase::candidate(&document)
                    {
                        font_suitcases.open(system, &document)
                                  .map_err(|cause| associations::OpenFailure { document, cause })
                    }
                    else
                    {
                        associations.begin(system, browser.startup, document)
                    };
                try_document_open(system, &mut alerts, result)?
            }
            else
            {
                true
            };
            if accepted && alerts.window() == WindowId(0)
            {
                let _ = try_document_open(system, &mut alerts, associations.advance(system))?;
            }
        }
        let completed = match dragging.advance(system, &mut browser)
        {
            Ok(completed) => completed,
            Err(error) =>
            {
                if let Some(failure) = dragging.take_failure()
                {
                    alerts.transfer_error(system, failure)?;
                }
                else
                {
                    let _ = try_action::<()>(system, &mut alerts, Err(error))?;
                }
                false
            }
        };
        if let Some(recovery) = dragging.take_recovery()
        {
            alerts.transfer_recovery(system, recovery)?;
        }
        if completed
        {
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
            if alerts.window() == WindowId(0)
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
        }
        if deferred_command.is_some() && dragging.saving_positions()
        {
            system.events().next(events::EventMask::from_bits_retain(0))?;
            continue;
        }
        let (event, position) = if let Some(input) =
            deferred_command.take().or_else(|| browser.next_deferred_pointer())
        {
            input
        }
        else
        {
            system.wait_positioned(mac_toolbox::events::EventMask::ALL,
                                   if info.pending()
                                      || browser.pending()
                                      || dragging.runnable()
                                      || dragging.positions_pending(&browser)
                                      || (associations.pending() && alerts.window() == WindowId(0))
                                   {
                                       0
                                   }
                                   else
                                   {
                                       30
                                   })?
                  .into_parts()
        };
        // Finish our owned background mutation before another storage command.
        // Pointer tracking remains live while that transaction is in flight.
        if dragging.saving_positions() && matches!(event, Event::MenuCommand { .. })
        {
            deferred_command = Some((event, position));
            continue;
        }
        alerts.set_context(&event);
        if dragging.ui_event(system, &event, position)
        {
            continue;
        }
        if let Event::WindowGeometryChanged { window,
                                              .. } = event
        {
            if !try_action(system, &mut alerts, font_suitcases.geometry_changed(system, window))?
                .unwrap_or(true)
            {
                let _ = try_action(system, &mut alerts, browser.geometry_changed(system, window))?;
            }
            continue;
        }
        if let Event::DisplayChanged { .. } = event
        {
            let _ = try_action(system, &mut alerts, browser.display_changed(system))?;
            continue;
        }
        if matches!(event, Event::Activate { .. }) && alerts.window() == WindowId(0)
        {
            browser.activate(&event);
            let _ = try_action(system, &mut alerts, browser.sync_desktop_activation(system))?;
        }
        if let Event::ProcessFileChanged { volume,
                                           id,
                                           running,
                                           .. } = event
        {
            let _ = try_action(system,
                               &mut alerts,
                               browser.application_file_changed(system,
                                                                browser::Location { volume,
                                                                                    folder:
                                                                                        id },
                                                                running))?;
            continue;
        }
        if let Event::Drag { .. } = event
        {
            let completed = matches!(&event,
                Event::Drag { offer: drag::DragEvent::Ended {
                    outcome: drag::DragOutcome::Moved | drag::DragOutcome::Copied }, .. });
            if alerts.window() != WindowId(0)
               && matches!(&event,
                           Event::Drag { offer: drag::DragEvent::Tracking(_)
                                                | drag::DragEvent::Dropped(_),
                                         .. })
            {
                // Dropping an ignored offer/payload rejects it through the SDK.
                dragging.cancel(&mut browser);
                drop(event);
            }
            else
            {
                let _ = try_action(system,
                                   &mut alerts,
                                   dragging.event(system, &mut browser, event, position))?;
            }
            if completed
            {
                startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            let confirm = startup.alert_event(system, &mut alerts, &event)?;
            if alerts.window() == WindowId(0)
            {
                if clipboard.window != WindowId(0)
                {
                    let _ = try_action(system, &mut alerts, clipboard.draw(system))?;
                }
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            if confirm
            {
                let _ = try_action(system, &mut alerts, browser.empty_trash(system))?;
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            continue;
        }
        if matches!(event,Event::Other{kind:events::classic::EventKind::OsEvent,message,..} if message & 0xff00_0000==0x0200_0000)
        {
            let _ = try_action(system, &mut alerts, browser.prune(system))?;
        }
        if browser.marquee_tracking()
           && matches!(event, Event::Idle { .. } | Event::MouseUp { .. } | Event::KeyDown { .. })
        {
            let _ = try_action(system, &mut alerts, browser.track(system, &event, position))?;
            if !browser.marquee_tracking()
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            continue;
        }
        if matches!(event, Event::Idle { .. })
        {
            startup_retry = startup.pending;
            if startup.pending && !startup.initialized
            {
                continue;
            }
            let _ = try_action(system, &mut alerts, browser.save_folder_states(system))?;
            if startup.pending && alerts.window() != WindowId(0)
            {
                continue;
            }
            let before = (browser.can_select_all(), browser.can_new_folder(system));
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
            if startup.pending && alerts.window() != WindowId(0)
            {
                continue;
            }
            let _ = try_action(system, &mut alerts, browser.advance_cache(system))?;
            if before != (browser.can_select_all(), browser.can_new_folder(system))
               && alerts.window() == WindowId(0)
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            if alerts.window() != WindowId(0)
            {
                continue;
            }
        }
        let _ = try_action(system, &mut alerts, clipboard.refresh(system))?;
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        if try_action(system, &mut alerts, font_suitcases.event(system, &event, position))?
            .unwrap_or(false)
        {
            if matches!(
                event,
                Event::Activate { .. }
                    | Event::CloseRequested { .. }
                    | Event::MenuCommand { .. }
                    | Event::Other { .. }
            ) || matches!(event, Event::KeyDown { .. })
                && event.input().modifiers.contains(events::Modifiers::COMMAND)
                && event.key().is_some_and(|key| key.character().eq_ignore_ascii_case(&'w'))
            {
                update_menus(system, &browser, &info)?;
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        let info_handled =
            try_action(system, &mut alerts, info.event(system, &event, position))?.unwrap_or(false);
        if info.take_changed()
        {
            // Native writes have already published exact parent notifications.
            // Consume those instead of repainting every cached folder.
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
        }
        if info_handled
        {
            // Typing/selection only changes Info's edit policy. The broader
            // Finder policy enumerates Trash and must not run per keystroke.
            if matches!(event, Event::KeyDown { .. } | Event::KeyUp { .. } | Event::Idle { .. })
               || matches!(event,
                           Event::MenuCommand { menu: MenuId(1253),
                                                .. })
            {
                startup.update_info_menus(system, &mut alerts, &info)?;
            }
            else
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        if try_action(system, &mut alerts, clipboard.event(system, &event, position))?.unwrap_or(false)
        {
            if startup.pending
            {
                if !matches!(event, Event::Idle { .. })
                {
                    startup.update_menus(system, &mut alerts, &browser, &info)?;
                }
            }
            else
            {
                menus::update_after_event(system, &browser, &event)?;
                if !matches!(event, Event::Idle { .. }) { info.update_menus(system)?; }
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        let _ = try_action(system, &mut alerts, browser.track(system, &event, position))?;
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        if let Some(selection) = system.menus().selection(&event)?
        {
            let mut log = browser::Text::<120>::default();
            use core::fmt::Write;
            let _ = writeln!(&mut log,
                             "FINDER_MENU menu={} item={} command={:08x}",
                             selection.menu_id, selection.item, selection.command);
            system.log(log.as_str())?;
            let action = match selection.command
            {
                menus::NEW_FOLDER => browser.new_folder(system),
                menus::OPEN =>
                {
                    try_open_action(system, &mut alerts, browser.open_selection(system)).map(|_| ())
                }
                menus::SELECT_ALL => browser.select_all(system),
                menus::GET_INFO => info.open_selection(system, browser.selected_locations()),
                menus::CLOSE =>
                {
                    if let Some(front) = browser.closable_front(system)
                    {
                        system.close_window(front)?;
                    }
                    browser.prune(system)
                }
                menus::PUT_AWAY => browser.put_away(system),
                menus::CLEAR => browser.trash_selection(system),
                menus::EMPTY => alerts.confirm_empty(system, &browser),
                menus::SHOW_CLIPBOARD => clipboard.open(system),
                menus::RESTART | menus::SHUTDOWN =>
                {
                    let result = if selection.command == menus::RESTART
                    {
                        system.power().restart()
                    }
                    else
                    {
                        system.power().shutdown()
                    };
                    match result
                    {
                        Ok(()) => Ok(()),
                        Err(error) => alerts.power_error(system, error)
                    }
                }
                _ => Ok(())
            };
            let _ = try_action(system, &mut alerts, action)?;
            if alerts.window() == WindowId(0)
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            continue;
        }
        let mut click_handled = false;
        match event
        {
            Event::Update { .. } =>
            {
                if browser.contains(event.window().unwrap_or_default())
                {
                    let _ = try_action(system,
                                       &mut alerts,
                                       browser.draw(system, event.window().unwrap_or_default()))?;
                }
            }
            Event::Activate { .. } =>
            {
                browser.activate(&event);
                // Window Manager paints activation chrome and queues damage for
                // affected content. Redraw each window when its Update arrives,
                // rather than rebuilding every folder on both Activate edges.
            }
            Event::KeyDown { .. } =>
            {
                let key = event.key().map_or('\0', |key| key.character()).to_ascii_lowercase();
                let code = event.key().map_or(0, |key| key.code());
                if key == 'q'
                   && !event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let launch = (|| {
                        let app = system.files().lookup(browser.startup,
                                                         files::ROOT_ID,
                                                         "/Applications/QuickDraw Test")?;
                        system.processes().launch(browser.startup, app.id)?;
                        Ok(())
                    })();
                    let _ = try_action(system, &mut alerts, launch)?;
                }
                else if key == 'a'
                          && !event.input()
                                   .modifiers
                                   .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let _ = try_action(system, &mut alerts, system.menus().about_system())?;
                }
                else if key == 'i'
                          && event.input()
                                  .modifiers
                                  .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    // A shortcut captured before the selection's menu policy
                    // updates arrives as KeyDown. Resolve it from current state.
                    let _ = try_action(system,
                                       &mut alerts,
                                       info.open_selection(system, browser.selected_locations()))?;
                }
                else if key == 'w'
                          && event.input()
                                  .modifiers
                                  .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    if let Some(front) = browser.closable_front(system)
                    {
                        system.close_window(front)?;
                        let _ = try_action(system, &mut alerts, browser.prune(system))?;
                    }
                }
                else if key == 'n'
                          && !event.input()
                                   .modifiers
                                   .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let _ =
                        try_action(system,&mut alerts,
                                   browser.open(system,
                                         browser::Location { volume: browser.startup,
                                                             folder: mac_toolbox::files::ROOT_ID }))?;
                }
                else if code == 0x33
                          && event.input()
                                  .modifiers
                                  .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let _ = try_action(system, &mut alerts, browser.trash_selection(system))?;
                }
                else if code == 0x7e
                          && event.input()
                                  .modifiers
                                  .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let _ = try_action(system, &mut alerts, browser.parent(system))?;
                }
                else if key == '\r'
                          || key == 'o'
                             && event.input()
                                     .modifiers
                                     .contains(mac_toolbox::events::Modifiers::COMMAND)
                          || code == 0x7d
                             && event.input()
                                     .modifiers
                                     .contains(mac_toolbox::events::Modifiers::COMMAND)
                {
                    let _ = try_open_action(system, &mut alerts, browser.open_selection(system))?;
                }
                else if code == 0x74
                {
                    let _ = try_action(system, &mut alerts, browser.scroll(system, -104))?;
                }
                else if code == 0x79
                {
                    let _ = try_action(system, &mut alerts, browser.scroll(system, 104))?;
                }
            }
            Event::MouseDown { .. }
                if event.window().is_none()
                   || browser.contains(event.window().unwrap_or_default()) =>
            {
                match try_open_action(system, &mut alerts, browser.click(system, &event, position))?
                {
                    Some(browser::ClickOutcome::Completed) =>
                    {
                        let _ =
                            try_action(system, &mut alerts, dragging.begin(system, &mut browser))?;
                        click_handled = true;
                    }
                    Some(browser::ClickOutcome::Deferred) | None => ()
                }
            }
            _ =>
            {}
        }
        if alerts.window() == WindowId(0) && !matches!(event, Event::Idle { .. })
        {
            startup.update_menus(system, &mut alerts, &browser, &info)?;
        }
        if click_handled
        {
            // Selection, drag admission and menu policy are all committed.
            system.log("FINDER_CLICK_HANDLED\n")?;
        }
    }
}
