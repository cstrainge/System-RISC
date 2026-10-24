//! Interactive public Toolbox cursor and automatic TextEdit hover exerciser.
#![no_std]
#![no_main]


use mac_toolbox::{quickdraw::{cursors::StandardCursor, fonts::FontSelection},
                  textedit::{CursorMovement, EditorId},
                  *};

const REGION_LOGS: [&str; 9] =
    ["CURSOR_TEST_REGION kind=arrow\n", "CURSOR_TEST_REGION kind=ibeam\n",
     "CURSOR_TEST_REGION kind=crosshair\n", "CURSOR_TEST_REGION kind=plus\n",
     "CURSOR_TEST_REGION kind=watch\n", "CURSOR_TEST_REGION kind=animated-watch\n",
     "CURSOR_TEST_REGION kind=ring\n", "CURSOR_TEST_REGION kind=ram-disk\n",
     "CURSOR_TEST_REGION kind=automatic\n"];
const FONT: FontSelection = FontSelection { family: 0,
                                            size:   12,
                                            face:   0 };

fn label<'a>(system: &System<'_>, index: usize, buffer: &'a mut [u8]) -> Result<&'a str, Error>
{
    system.resources().application().indexed_string(2000, index as u32, buffer)
}

fn area(width: i16, index: usize) -> Rect
{
    let cell = (width - 34) / 4;
    let left = 12 + index as i16 % 4 * (cell + 3);
    let top = 42 + index as i16 / 4 * 74;
    Rect::new(left, top, left + cell, top + 64)
}

fn editor_view(width: i16, height: i16) -> Rect
{
    Rect::new(14, 210, width - 30, height - 32)
}

fn window_layout(system: &System<'_>) -> Result<(Rect, i16), Error>
{
    let resource = system.resources()
                         .application()
                         .get(resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"WIND"),
                                                       id:   2003 })?;
    let mut bytes = [0; 20];
    let read = if resource.size() == bytes.len()
    {
        resource.read(0, &mut bytes)
    }
    else
    {
        Err(Error::BadArgument)
    };
    resource.release()?;
    if read? != bytes.len()
    {
        return Err(Error::BadArgument);
    }
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    let procedure = word(8);
    if bounds.is_empty()
       || bounds.right.checked_sub(bounds.left).is_none_or(|width| width < 500)
       || bounds.bottom.checked_sub(bounds.top).is_none_or(|height| height < 320)
       || procedure != 4
    {
        return Err(Error::BadArgument);
    }
    Ok((bounds, procedure))
}

fn draw(system: &System<'_>, window: WindowId, editor: EditorId) -> Result<(), Error>
{
    let bounds = system.windows().info(window)?.bounds;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let view = editor_view(width, height);
    let info = system.textedit().info(editor)?;
    if info.dest != view || info.view != view
    {
        system.textedit().set_rects(editor, view, view)?;
    }
    system.windows().begin_update(window)?;
    let port = system.quickdraw().graphics(window);
    port.reset()?;
    port.colors(0xffffff, 0xffffff)?;
    port.paint_rect(Rect::new(0, 0, width, height))?;
    let mut buffer = [0; 256];
    port.text(FONT,
              Point { h: 12,
                      v: 22 },
              label(system, 0, &mut buffer)?,
              0)?;
    for index in 0..8
    {
        let bounds = area(width, index);
        port.colors(0, 0xffffff)?;
        port.frame_rect(bounds)?;
        port.text(FONT,
                  Point { h: bounds.left + 6,
                          v: bounds.top + 34 },
                  label(system, index + 1, &mut buffer)?,
                  0)?;
    }
    port.text(FONT,
              Point { h: 12,
                      v: 201 },
              label(system, 9, &mut buffer)?,
              0)?;
    port.frame_rect(Rect::new(view.left - 2, view.top - 2, view.right + 2, view.bottom + 2))?;
    system.textedit().draw(editor, window)?;
    port.text(FONT,
              Point { h: 12,
                      v: height - 9 },
              label(system, 10, &mut buffer)?,
              0)?;
    system.windows().end_update(window)
}

fn choose(system: &System<'_>, region: usize) -> Result<(), Error>
{
    let cursor = system.quickdraw().cursors();
    match region
    {
        0 => cursor.set_standard(StandardCursor::Arrow),
        1 => cursor.set_standard(StandardCursor::IBeam),
        2 => cursor.set_standard(StandardCursor::Crosshair),
        3 => cursor.set_standard(StandardCursor::Plus),
        4 => cursor.set_standard(StandardCursor::Watch),
        5 => cursor.system().animate_resource(-6078, 6),
        6 => cursor.system().set_resource(-5760),
        7 => cursor.system().set_resource(-20161),
        _ => cursor.init()
    }?;
    // A zero-mask poll presents resident cursor changes without consuming any
    // queued application input. The following marker acknowledges visible work.
    system.events().next(mac_toolbox::events::EventMask::from_bits_retain(0))?;
    system.log(REGION_LOGS[region])
}

fn run(system: &System<'_>) -> Result<(), Error>
{
    system.menus().clear()?;
    let _menus = [system.menus().install_owned(mac_toolbox::resources::APPLICATION, 128)?,
                  system.menus().install_owned(mac_toolbox::resources::APPLICATION, 129)?];
    let mut buffer = [0; 128];
    let title = system.resources().application().string(128, &mut buffer)?;
    let (bounds, procedure) = window_layout(system)?;
    let mut window_owner =
        system.windows()
              .create_owned(windows::WindowSpec::new(bounds, title).procedure(procedure)
                                                                   .minimum(500, 320))?;
    let window = window_owner.id();
    system.windows().set_grow_icon_clip(window, Some(Rect::ZERO))?;
    let view = editor_view(bounds.right - bounds.left, bounds.bottom - bounds.top);
    let mut editor_owner = system.textedit().create_owned(view, view)?;
    let editor = editor_owner.id();
    system.textedit().set_text(editor, "Try typing here.\rThe I-beam follows this text area.")?;
    system.textedit().activate(editor, true)?;
    draw(system, window, editor)?;
    system.log("CURSOR_TEST_READY\n")?;
    let mut region = usize::MAX;
    let mut tracking = false;
    let mut editable = true;
    let mut last_mouse = Point::default();
    loop
    {
        let (event, position) =
            system.wait_positioned(mac_toolbox::events::EventMask::ALL, 6)?.into_parts();
        if let Some(selection) = system.menus().selection(&event)?
        {
            if [u32::from_be_bytes(*b"clos"), u32::from_be_bytes(*b"quit")]
               .contains(&selection.command)
            {
                window_owner.close()?;
                break;
            }
            let cursor = system.quickdraw().cursors();
            let action = match selection.command.to_be_bytes()
            {
                [b'h', b'i', b'd', b'e'] =>
                {
                    cursor.hide()?;
                    "CURSOR_TEST_ACTION kind=hide\n"
                }
                [b's', b'h', b'o', b'w'] =>
                {
                    cursor.show()?;
                    "CURSOR_TEST_ACTION kind=show\n"
                }
                [b'o', b'b', b's', b'c'] =>
                {
                    cursor.obscure()?;
                    "CURSOR_TEST_ACTION kind=obscure\n"
                }
                [b'r', b's', b'e', b't'] =>
                {
                    cursor.init()?;
                    "CURSOR_TEST_ACTION kind=reset\n"
                }
                [b'n', b'e', b'x', b't'] =>
                {
                    if cursor.info()?.flags & quickdraw::cursors::CURSOR_ANIMATED != 0
                    {
                        cursor.advance_animation()?;
                    }
                    "CURSOR_TEST_ACTION kind=next-frame\n"
                }
                [b'e', b'd', b'i', b't'] =>
                {
                    editable = !editable;
                    system.textedit().set_cursor_enabled(editor, editable)?;
                    let active = system.windows().front()? == Some(window);
                    system.textedit().activate(editor, active && editable)?;
                    tracking = false;
                    draw(system, window, editor)?;
                    if editable
                    {
                        "CURSOR_TEST_ACTION kind=editable\n"
                    }
                    else
                    {
                        "CURSOR_TEST_ACTION kind=read-only\n"
                    }
                }
                _ => continue
            };
            system.events().next(mac_toolbox::events::EventMask::from_bits_retain(0))?;
            system.log(action)?;
        }
        let bounds = match system.windows().info(window)
        {
            Ok(info) => info.bounds,
            Err(Error::NotFound) => break,
            Err(error) => return Err(error)
        };
        let active = system.windows().front()? == Some(window);
        let local =
            position.window().filter(|p| p.window() == window).map(|p| p.point()).transpose()?;
        let local = local.unwrap_or(Point { h: -1,
                                            v: -1 });
        let mut redraw =
            matches!(event, Event::Update { .. }) && event.window().unwrap_or_default() == window;
        let mut edited = false;
        if matches!(event, Event::Activate { .. })
        {
            system.textedit().activate(editor, active && editable)?;
            tracking = false;
            redraw = true;
        }
        let view = editor_view(bounds.right - bounds.left, bounds.bottom - bounds.top);
        if active && editable
        {
            match event
            {
                Event::MouseDown { .. }
                    if event.window().unwrap_or_default() == window && view.contains(local) =>
                {
                    system.textedit()
                          .click(editor,
                                 local,
                                 event.input()
                                      .modifiers
                                      .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                    tracking = true;
                    redraw = true;
                }
                Event::MouseUp { .. } => tracking = false,
                Event::KeyDown { .. }
                    if !event.input()
                             .modifiers
                             .contains(mac_toolbox::events::Modifiers::COMMAND) =>
                {
                    let movement = match event.key().map_or(0, |key| key.code())
                    {
                        0x7b => Some(CursorMovement::Left),
                        0x7c => Some(CursorMovement::Right),
                        0x7e => Some(CursorMovement::Up),
                        0x7d => Some(CursorMovement::Down),
                        0x73 => Some(CursorMovement::LineStart),
                        0x77 => Some(CursorMovement::LineEnd),
                        _ => None
                    };
                    if let Some(movement) = movement
                    {
                        system.textedit()
                              .move_cursor(editor,
                                           movement,
                                           event.input()
                                                .modifiers
                                                .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                        redraw = true;
                        edited = true;
                    }
                    else
                    {
                        let key = event.key().map_or('\0', |key| key.character());
                        if !key.is_control() || key == '\r' || key == '\u{8}'
                        {
                            system.textedit().key(editor, key)?;
                            redraw = true;
                            edited = true;
                        }
                    }
                }
                Event::Idle { .. } =>
                {
                    if tracking && event.input().at != last_mouse
                    {
                        system.textedit().click(editor, local, true)?;
                        redraw = true;
                    }
                    if !redraw
                    {
                        redraw = system.textedit().blink(editor, window, event.input().when)?;
                    }
                }
                _ =>
                {}
            }
        }
        last_mouse = event.input().at;
        // Input wakes WaitNextEvent on pointer motion. Only region transitions
        // issue a cursor request, so idle polling never restarts animation.
        if !event.input().modifiers.button_down()
           && matches!(event,
                       Event::Idle { .. }
                       | Event::MouseDown { .. }
                       | Event::MouseUp { .. }
                       | Event::Activate { .. })
        {
            let next = if active
            {
                (0..8).find(|&index| area(bounds.right - bounds.left, index).contains(local))
                      .unwrap_or(8)
            }
            else
            {
                8
            };
            if next != region
            {
                choose(system, next)?;
                region = next;
            }
        }
        if redraw
        {
            draw(system, window, editor)?;
        }
        if edited
        {
            system.log("CURSOR_TEST_EDIT\n")?;
        }
    }
    editor_owner.dispose()?;
    system.quickdraw().cursors().init()
}

/// # Safety
/// The native loader supplies initialized segments and a live service table.
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
    match run(&system)
    {
        Ok(()) => mac_toolbox::abi::OK,
        Err(status) =>
        {
            let _ = system.log("CURSOR_TEST_ERROR\n");
            status.status()
        }
    }
}

mac_toolbox::panic_handler!();
