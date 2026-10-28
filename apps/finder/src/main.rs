//! Finder is a separate resource-fork application using System Toolbox services.
#![no_std]
#![no_main]

use mac_toolbox::{System, *};
include_resource_ids!();

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

fn update_menus(system: &System<'_>, browser: &browser::Browser, info: &get_info::Controller)
                -> Result<(), Error>
{
    menus::update(system, browser)?;
    info.update_menus(system)
}

fn try_action<'a, T>(system: &System<'a>, alerts: &mut ui::Alerts<'a>, result: Result<T, Error>)
                     -> Result<Option<T>, Error>
{
    use core::fmt::Write;
    match result
    {
        Ok(value) => Ok(Some(value)),
        Err(status) =>
        {
            let mut message = browser::Text::<80>::default();
            let _ = writeln!(&mut message,
                             "FINDER_ACTION_FAILED status={} error={status:?}",
                             status.status());
            system.log(message.as_str())?;
            if let Err(error) = alerts.error(system, status)
            {
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error.status());
                system.log(message.as_str())?;
            }
            Ok(None)
        }
    }
}

fn about(system: &System<'_>, id: WindowId) -> Result<(), Error>
{
    system.begin_update(id)?;
    let port = system.quickdraw().graphics(id);
    port.reset()?;
    port.colors(0xffffff, 0xffffff)?;
    port.paint_rect(Rect::new(0, 0, 300, 210))?;
    for (y, index) in [(20, 4), (45, 5), (70, 6), (100, 7), (120, 8), (140, 9), (160, 10)]
    {
        let mut bytes = [0; 768];
        let text = ui::text(system, index, &mut bytes)?;
        port.text(mac_toolbox::quickdraw::fonts::FontSelection { family: 3,
                                                                 size:   9,
                                                                 face:   0 },
                  Point { h: 16,
                          v: y },
                  text,
                  0)?;
    }
    system.end_update(id)
}
fn open_about(system: &System<'_>, id: &mut WindowId) -> Result<(), Error>
{
    if *id == WindowId(0) || system.window(*id).is_err()
    {
        let mut bytes = [0; 768];
        let title = ui::text(system, 3, &mut bytes)?;
        *id = system.new_window(Rect::new(250, 150, 550, 345), title, (260, 180), 0)?;
    }
    system.select_window(*id)?;
    about(system, *id)
}
fn finder(system: &System<'_>) -> Result<(), Error>
{
    let mut browser = browser::Browser::default();
    let mut dragging = browser::dragging::Controller::default();
    let mut watches = watches::Controller::default();
    let mut associations = associations::Controller::default();
    let mut info = get_info::Controller::default();
    let _menus = menus::install(system)?;
    let mut alerts = ui::Alerts::default();
    let _ = try_action(system, &mut alerts, browser.initialize(system))?;
    if alerts.window() == WindowId(0)
    {
        update_menus(system, &browser, &info)?;
    }
    let mut about_id = WindowId(0);
    let mut clipboard = clipboard::Viewer::default();
    let _ = try_action(system, &mut alerts, watches.poll(system, &mut browser))?;
    loop
    {
        let _ = try_action(system, &mut alerts, browser.advance_cache(system))?;
        let (event, _position) =
            system.next_positioned(mac_toolbox::events::EventMask::ALL)?.into_parts();
        alerts.set_context(&event);
        if let Event::WindowGeometryChanged { window,
                                              .. } = event
        {
            let _ = try_action(system, &mut alerts, browser.geometry_changed(system, window))?;
            continue;
        }
        if matches!(event, Event::Activate { .. })
        {
            browser.activate(&event);
        }
        if matches!(event, Event::Update { .. })
           && browser.contains(event.window().unwrap_or_default())
        {
            let _ = try_action(system,
                               &mut alerts,
                               browser.draw(system, event.window().unwrap_or_default()))?;
        }
        if matches!(event, Event::Idle { .. }) && !browser.pending()
        {
            break;
        }
    }
    if alerts.window() == WindowId(0)
    {
        update_menus(system, &browser, &info)?;
    }
    system.log("DESKTOP_READY: Finder and native Window Manager\n")?;
    let mut deferred_command = None;
    loop
    {
        if alerts.window() == WindowId(0)
        {
            if let Some(document) = browser.take_document_open()
            {
                let _ = try_action(system,
                                   &mut alerts,
                                   associations.begin(system, browser.startup, document))?;
            }
            let _ = try_action(system, &mut alerts, associations.advance(system))?;
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
            let _ = try_action(system, &mut alerts, watches.poll(system, &mut browser))?;
            if alerts.window() == WindowId(0)
            {
                update_menus(system, &browser, &info)?;
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
            let _ = try_action(system, &mut alerts, browser.geometry_changed(system, window))?;
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
                let _ = try_action(system, &mut alerts, watches.poll(system, &mut browser))?;
                update_menus(system, &browser, &info)?;
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            let confirm = alerts.event(system, &event)?;
            if alerts.window() == WindowId(0)
            {
                if clipboard.window != WindowId(0)
                {
                    let _ = try_action(system, &mut alerts, clipboard.draw(system))?;
                }
                update_menus(system, &browser, &info)?;
            }
            if confirm
            {
                let _ = try_action(system, &mut alerts, browser.empty_trash(system))?;
                update_menus(system, &browser, &info)?;
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
                update_menus(system, &browser, &info)?;
            }
            continue;
        }
        if matches!(event, Event::Idle { .. })
        {
            let _ = try_action(system, &mut alerts, browser.save_folder_states(system))?;
            let before = (browser.can_select_all(), browser.can_new_folder(system));
            let _ = try_action(system, &mut alerts, watches.poll(system, &mut browser))?;
            let _ = try_action(system, &mut alerts, browser.advance_cache(system))?;
            if before != (browser.can_select_all(), browser.can_new_folder(system))
               && alerts.window() == WindowId(0)
            {
                update_menus(system, &browser, &info)?;
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
        let info_handled =
            try_action(system, &mut alerts, info.event(system, &event, position))?.unwrap_or(false);
        if info.take_changed()
        {
            // Native writes have already published exact parent notifications.
            // Consume those instead of repainting every cached folder.
            let _ = try_action(system, &mut alerts, watches.poll(system, &mut browser))?;
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
                info.update_menus(system)?;
            }
            else
            {
                update_menus(system, &browser, &info)?;
            }
            continue;
        }
        if alerts.window() != WindowId(0)
        {
            continue;
        }
        if try_action(system, &mut alerts, clipboard.event(system, &event, position))?.unwrap_or(false)
        {
            menus::update_after_event(system, &browser, &event)?;
            if !matches!(event, Event::Idle { .. })
            {
                info.update_menus(system)?;
            };
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
                menus::OPEN => browser.open_selection(system),
                menus::SELECT_ALL => browser.select_all(system),
                menus::GET_INFO => info.open_selection(system, browser.selected_locations()),
                menus::CLOSE =>
                {
                    if let Ok(front) = system.window(WindowId(0))
                    {
                        system.close_window(front.id)?;
                    }
                    browser.prune(system)
                }
                menus::PUT_AWAY => browser.put_away(system),
                menus::CLEAR => browser.trash_selection(system),
                menus::EMPTY => alerts.confirm_empty(system, &browser),
                menus::ABOUT => open_about(system, &mut about_id),
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
                update_menus(system, &browser, &info)?;
            }
            continue;
        }
        let mut click_handled = false;
        match event
        {
            Event::Update { .. } =>
            {
                if event.window().unwrap_or_default() == about_id && about_id != WindowId(0)
                {
                    about(system, about_id)?;
                }
                else if browser.contains(event.window().unwrap_or_default())
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
                    let _ = try_action(system, &mut alerts, open_about(system, &mut about_id))?;
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
                    if let Ok(front) = system.window(WindowId(0))
                    {
                        system.close_window(front.id)?;
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
                    let _ = try_action(system, &mut alerts, browser.open_selection(system))?;
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
                match try_action(system, &mut alerts, browser.click(system, &event, position))?
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
            update_menus(system, &browser, &info)?;
        }
        if click_handled
        {
            // Selection, drag admission and menu policy are all committed.
            system.log("FINDER_CLICK_HANDLED\n")?;
        }
    }
}

/// # Safety
/// Loader supplies initialized ELF segments and resident System services.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status.status()
    };
    if let Err(status) = system.log("Finder: native application entry reached\n")
    {
        return status.status();
    }
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return mac_toolbox::abi::OK;
    }
    match finder(&system)
    {
        Ok(()) => mac_toolbox::abi::OK,
        Err(status) =>
        {
            use core::fmt::Write;
            let mut line = browser::Text::<160>::default();
            let _ = writeln!(&mut line, "FINDER_ERROR status={} error={status:?}", status.status());
            let _ = system.log(line.as_str());
            status.status()
        }
    }
}
mac_toolbox::panic_handler!();
