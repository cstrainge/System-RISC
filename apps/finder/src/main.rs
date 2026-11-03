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

fn try_folder_action<'a, T>(system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                            result: Result<T, Error>, retry: Option<browser::FailedFolder>)
                            -> Result<Option<T>, Error>
{
    if let Some(failed) = retry
    {
        match result
        {
            Ok(value) => Ok(Some(value)),
            Err(status) =>
            {
                use core::fmt::Write;
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message,
                                 "FINDER_ACTION_FAILED status={} error={status:?}",
                                 status.status());
                let _ = system.log(message.as_str());
                if let Err(error) = alerts.folder_error(system, status, failed)
                {
                    let mut message = browser::Text::<80>::default();
                    let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
                    let _ = system.log(message.as_str());
                }
                Ok(None)
            }
        }
    }
    else
    {
        try_action(system, alerts, result)
    }
}

fn try_open_action<'a, T>(system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                          result: Result<T, browser::ActionFailure>,
                          browser: &mut browser::Browser)
                          -> Result<Option<T>, Error>
{
    use core::fmt::Write;
    match result
    {
        Ok(value) => Ok(Some(value)),
        Err(browser::ActionFailure::Other(cause)) =>
        {
            let target = browser.take_open_failure(Some(cause));
            try_folder_action(system, alerts, Err(cause), target)
        }
        Err(browser::ActionFailure::Open { item,
                                           cause }) =>
        {
            if let Err(error) = alerts.item_open_error(system, item, cause)
            {
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
                let _ = system.log(message.as_str());
            }
            Ok(None)
        }
    }
}

/// Startup debt survives error presentation. Successful stages keep their real
/// objects; only a successful native acknowledgement retires this state.
struct Startup<'a>
{
    menus:       Option<[mac_toolbox::menus::MenuRef<'a>; 5]>,
    initialized: bool,
    menus_ready: bool,
    pending:     bool
}
impl<'a> Startup<'a>
{
    fn new() -> Self
    {
        Self { menus:       None,
               initialized: false,
               menus_ready: false,
               pending:     true }
    }

    fn update_menus(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                    browser: &browser::Browser, info: &get_info::Controller)
                    -> Result<(), Error>
    {
        if self.menus.is_none() || !self.initialized
        {
            self.menus_ready = false;
            return Ok(());
        }
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

    fn alert_event(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
                   browser: &mut browser::Browser, event: &Event<'a>)
                   -> Result<Option<ui::Completion>, Error>
    {
        match alerts.event(system, browser, event)
        {
            Err(error) =>
            {
                // The notification owns its real Alert/item/menu progress for
                // its entire lifetime, including after desktop acknowledgement.
                self.menus_ready = false;
                use core::fmt::Write;
                let mut line = browser::Text::<160>::default();
                let _ = writeln!(&mut line,
                                 "FINDER_ALERT_EVENT_FAILED status={} error={error:?}",
                                 error.status());
                let _ = system.log(line.as_str());
                Ok(None)
            }
            result => result
        }
    }

    fn advance(&mut self, system: &System<'a>, alerts: &mut ui::Alerts<'a>,
               browser: &mut browser::Browser, info: &get_info::Controller, settled_turn: bool)
               -> Result<(), Error>
    {
        if !self.pending || alerts.active()
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
            alerts.menus_installed();
        }
        if !self.initialized
        {
            let result = browser.initialize(system);
            let target = browser.take_open_failure(result.as_ref().err().copied());
            self.initialized = try_folder_action(system, alerts, result, target)?.is_some();
            if !self.initialized
            {
                return Ok(());
            }
            self.menus_ready = false;
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
        if !self.menus_ready || alerts.active()
        {
            return Ok(());
        }
        if try_action(system, alerts, system.processes().finder_desktop_ready())?.is_some()
        {
            self.pending = false;
            browser.finish_startup_confirmation();
            let _ = system.log("DESKTOP_READY: Finder and native Window Manager\n");
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
                let _ = system.log(message.as_str());
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
    let mut retry_folder: Option<browser::FailedFolder> = None;
    let mut prune_pending = false;
    let mut modal_input_owned = false;
    loop
    {
        let mut serviced_update = false;
        if alerts.active() && !modal_input_owned
        {
            dragging.cancel(&mut browser);
            browser.take_modal_input();
            modal_input_owned = true;
            let result = browser.finish_modal_input(system);
            if result.is_err()
            {
                alerts.defer_service(system);
            }
            let _ = try_action(system, &mut alerts, result)?;
        }
        if !alerts.active()
        {
            modal_input_owned = false;
        }
        if prune_pending && !alerts.active()
        {
            let result = browser.prune(system);
            if result.is_ok()
            {
                prune_pending = false;
            }
            let _ = try_action(system, &mut alerts, result)?;
        }
        if !alerts.active()
           && !dragging.runnable()
           && !dragging.positions_pending(&browser)
           && let Some(failed) = retry_folder.take()
        {
            let result = browser.retry_failed_folder(system, failed);
            let target = result.as_ref()
                               .err()
                               .map(|_| browser.failed_folder(failed.window()).unwrap_or(failed));
            let _ = try_folder_action(system, &mut alerts, result, target)?;
        }
        if !alerts.active()
           && retry_folder.is_none()
           && !dragging.runnable()
           && !dragging.positions_pending(&browser)
           && let Some(window) = browser.take_deferred_update()
        {
            serviced_update = true;
            let result = browser.draw(system, window);
            if let Err(cause) = result
            {
                browser.retain_draw_failure(window, cause);
            }
            let target = result.as_ref().err().and_then(|_| browser.failed_folder(window));
            let _ = try_folder_action(system, &mut alerts, result, target)?;
        }
        if !serviced_update
           && !alerts.active()
           && retry_folder.is_none()
           && !dragging.runnable()
           && !dragging.positions_pending(&browser)
        {
            if clipboard.has_deferred_update()
            {
                let _ = try_action(system, &mut alerts, clipboard.draw_deferred(system))?;
            }
            else if info.has_deferred_update()
            {
                let _ = try_action(system, &mut alerts, info.draw_deferred(system))?;
            }
        }
        if startup_retry && !dragging.runnable() && !dragging.positions_pending(&browser)
        {
            startup_retry = false;
            startup.advance(system, &mut alerts, &mut browser, &info, true)?;
        }
        if !alerts.active() && retry_folder.is_none()
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
            if accepted && !alerts.active()
            {
                let _ = try_document_open(system, &mut alerts, associations.advance(system))?;
            }
        }
        let completed = if alerts.active()
        {
            false
        }
        else
        {
            match dragging.advance(system, &mut browser)
            {
                Ok(completed) => completed,
                Err(error) =>
                {
                    if let Some(failure) = dragging.take_failure()
                    {
                        let notice = alerts.transfer_error(system, failure);
                        let _ = try_action(system, &mut alerts, notice)?;
                    }
                    else
                    {
                        let _ = try_action::<()>(system, &mut alerts, Err(error))?;
                    }
                    false
                }
            }
        };
        if !alerts.active()
           && let Some(recovery) = dragging.take_recovery()
        {
            let notice = alerts.transfer_recovery(system, recovery);
            let _ = try_action(system, &mut alerts, notice)?;
        }
        if completed
        {
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
            if !alerts.active()
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
        }
        if !alerts.active() && deferred_command.is_some() && dragging.saving_positions()
        {
            system.events().next(events::EventMask::from_bits_retain(0))?;
            continue;
        }
        let deferred = if alerts.active() || retry_folder.is_some()
        {
            None
        }
        else
        {
            deferred_command.take().or_else(|| browser.next_deferred_pointer())
        };
        // A queued physical release must be dispatched once, never captured
        // into the pending-click queue again while that same queue drains.
        let replayed_pointer = deferred.is_some();
        let (event, position) = if let Some(input) = deferred
        {
            input
        }
        else
        {
            system.wait_positioned(alerts.event_mask(),
                                   if !alerts.active()
                                      && (info.pending()
                                          || browser.pending()
                                          || dragging.runnable()
                                          || dragging.positions_pending(&browser)
                                          || associations.pending())
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
        if !alerts.active()
           && dragging.saving_positions()
           && matches!(event, Event::MenuCommand { .. })
        {
            deferred_command = Some((event, position));
            continue;
        }
        alerts.set_context(&event);
        if (!alerts.active() || matches!(event, Event::FileTransferChanged { .. }))
           && dragging.ui_event(system, &event, position)
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
        if matches!(event, Event::Activate { .. }) && alerts.active()
        {
            if browser.contains(event.window().unwrap_or_default())
            {
                browser.activate(&event);
            }
        }
        else if matches!(event, Event::Activate { .. })
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
            if alerts.active()
               && matches!(&event,
                           Event::Drag { offer: drag::DragEvent::Tracking(_)
                                                | drag::DragEvent::Dropped(_),
                                         .. })
            {
                // Dropping an ignored offer/payload rejects it through the SDK.
                drop(event);
            }
            else
            {
                let _ = try_action(system,
                                   &mut alerts,
                                   dragging.event(system, &mut browser, event, position))?;
            }
            if completed && !alerts.active()
            {
                startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
                if !alerts.active()
                {
                    startup.update_menus(system, &mut alerts, &browser, &info)?;
                }
            }
            continue;
        }
        if matches!(event,Event::Other{kind:events::classic::EventKind::OsEvent,message,..} if message & 0xff00_0000==0x0200_0000)
        {
            // A modal cannot consume the sole real close notification.
            prune_pending = true;
            let _ = try_action(system, &mut alerts, info.event(system, &event, position))?;
            continue;
        }
        if alerts.active()
           && matches!(event, Event::Update { .. })
           && browser.contains(event.window().unwrap_or_default())
        {
            let result = browser.defer_update(system, event.window().unwrap_or_default());
            if result.is_err()
            {
                alerts.defer_service(system);
            }
            let _ = try_action(system, &mut alerts, result)?;
            continue;
        }
        if alerts.active() && matches!(event, Event::Update { .. })
        {
            let window = event.window().unwrap_or_default();
            if window == clipboard.window && clipboard.window != WindowId(0)
            {
                let result = clipboard.defer_update(system, window);
                if result.is_err()
                {
                    alerts.defer_service(system);
                }
                let _ = try_action(system, &mut alerts, result)?;
                continue;
            }
            let deferred = info.defer_update(system, window);
            match deferred
            {
                Ok(true) => continue,
                Ok(false) => (),
                Err(cause) =>
                {
                    alerts.defer_service(system);
                    let _ = try_action::<()>(system, &mut alerts, Err(cause))?;
                    continue;
                }
            }
        }
        if alerts.active()
        {
            let completion = startup.alert_event(system, &mut alerts, &mut browser, &event)?;
            match completion
            {
                Some(ui::Completion::EmptyTrash) =>
                {
                    let _ = try_action(system, &mut alerts, browser.empty_trash(system))?;
                }
                Some(ui::Completion::RetryFolder(failed)) => retry_folder = Some(failed),
                _ => ()
            }
            if !alerts.active()
            {
                if clipboard.window != WindowId(0)
                {
                    let _ = try_action(system, &mut alerts, clipboard.draw(system))?;
                }
                if !alerts.active()
                {
                    let result = startup.update_menus(system, &mut alerts, &browser, &info);
                    let _ = try_action(system, &mut alerts, result)?;
                }
            }
            continue;
        }
        // Preserve the one acknowledged target while its existing storage
        // owner drains. Do not replace it with a later folder notification.
        if retry_folder.is_some()
        {
            continue;
        }
        if browser.marquee_tracking()
           && matches!(event, Event::Idle { .. } | Event::MouseUp { .. } | Event::KeyDown { .. })
        {
            let _ = try_action(system,
                               &mut alerts,
                               browser.track(system, &event, position, replayed_pointer))?;
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
            if alerts.active()
            {
                continue;
            }
            let before = (browser.can_select_all(), browser.can_new_folder(system));
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
            if alerts.active()
            {
                continue;
            }
            let result = browser.advance_cache(system);
            let target = result.as_ref().err().and_then(|_| browser.failed_cache());
            let _ = try_folder_action(system, &mut alerts, result, target)?;
            if before != (browser.can_select_all(), browser.can_new_folder(system))
               && !alerts.active()
            {
                startup.update_menus(system, &mut alerts, &browser, &info)?;
            }
            if alerts.active()
            {
                continue;
            }
        }
        let _ = try_action(system, &mut alerts, clipboard.refresh(system))?;
        if alerts.active()
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
        if alerts.active()
        {
            continue;
        }
        let info_handled =
            try_action(system, &mut alerts, info.event(system, &event, position))?.unwrap_or(false);
        if alerts.active()
        {
            continue;
        }
        if info.take_changed()
        {
            // Native writes have already published exact parent notifications.
            // Consume those instead of repainting every cached folder.
            startup.poll_watches(system, &mut alerts, &mut watches, &mut browser)?;
        }
        if alerts.active()
        {
            continue;
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
        if alerts.active()
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
        if alerts.active()
        {
            continue;
        }
        let _ = try_action(system,
                           &mut alerts,
                           browser.track(system, &event, position, replayed_pointer))?;
        if alerts.active()
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
            let _ = system.log(log.as_str());
            let action = match selection.command
            {
                menus::NEW_FOLDER => browser.new_folder(system),
                menus::OPEN =>
                {
                    try_open_action(system,
                                    &mut alerts,
                                    browser.open_selection(system),
                                    &mut browser).map(|_| ())
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
            let target = browser.take_open_failure(action.as_ref().err().copied());
            let _ = try_folder_action(system, &mut alerts, action, target)?;
            if !alerts.active()
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
                    let window = event.window().unwrap_or_default();
                    let result = browser.draw(system, window);
                    let target = result.as_ref().err().and_then(|_| browser.failed_folder(window));
                    let _ = try_folder_action(system, &mut alerts, result, target)?;
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
                    let result = browser.open(system,
                                       browser::Location { volume: browser.startup,
                                                           folder: mac_toolbox::files::ROOT_ID });
                    let target = browser.take_open_failure(result.as_ref().err().copied());
                    let _ = try_folder_action(system, &mut alerts, result, target)?;
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
                    let result = browser.parent(system);
                    let target = browser.take_open_failure(result.as_ref().err().copied());
                    let _ = try_folder_action(system, &mut alerts, result, target)?;
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
                    let _ = try_open_action(system,
                                            &mut alerts,
                                            browser.open_selection(system),
                                            &mut browser)?;
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
                match try_open_action(system,
                                      &mut alerts,
                                      browser.click(system, &event, position),
                                      &mut browser)?
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
        if !alerts.active() && !matches!(event, Event::Idle { .. })
        {
            startup.update_menus(system, &mut alerts, &browser, &info)?;
        }
        if click_handled
        {
            // Selection, drag admission and menu policy are all committed.
            let _ = system.log("FINDER_CLICK_HANDLED\n");
        }
    }
}
