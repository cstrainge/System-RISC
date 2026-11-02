//! Read-only font-file snapshots. Inspection and font decoding belong to System;
//! opening this Finder window never installs the source's families.
use core::fmt::Write;

use mac_toolbox::{controls::{ScrollBox, ScrollBoxSpec},
                  quickdraw::fonts::{FontSelection, FontSuitcase},
                  *};

use crate::browser::Location;

const CAPACITY: usize = 8;
const MINIMUM: (i16, i16) = (200, 100);
const FONT: FontSelection = FontSelection { family: 3,
                                            size:   12,
                                            face:   0 };

pub fn candidate(entry: &files::CatalogInfo) -> bool
{
    !entry.is_directory()
    && (entry.file_type == TypeCode::new(*b"FFIL")
        || entry.file_type == TypeCode::new(*b"sfnt")
        || entry.name().rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("ttf")))
}

struct Viewer<'a>
{
    // Managed controls and the inspection snapshot precede their parent window.
    list:     ScrollBox<'a>,
    suitcase: FontSuitcase<'a>,
    identity: Location,
    size:     Point,
    window:   windows::Window<'a>
}

#[derive(Default)]
pub struct Controller<'a>
{
    windows: [Option<Viewer<'a>>; CAPACITY]
}

fn layout(system: &System<'_>) -> ToolboxResult<(Rect, i16)>
{
    let resource =
        system.resources().application().get(ResourceKey { kind: TypeCode::new(*b"WIND"),
                                                            id:   2003 })?;
    let mut bytes = [0; 20];
    if resource.size() != bytes.len() || resource.read(0, &mut bytes)? != bytes.len()
    {
        return Err(Error::CorruptData);
    }
    resource.release()?;
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    let procedure = word(8);
    if bounds.right - bounds.left < MINIMUM.0
       || bounds.bottom - bounds.top < MINIMUM.1
       || procedure != 8
       || bytes[10..] != [1, 0, 1, 0, 0, 0, 0, 0, 0, 0]
    {
        return Err(Error::CorruptData);
    }
    Ok((bounds, procedure))
}

fn spec(size: Point, count: u32) -> ScrollBoxSpec
{
    ScrollBoxSpec { viewport: Rect::new(1, 1, size.h - 16, size.v - 1),
                    scrollbar: Rect::new(size.h - 16, 0, size.h, size.v - 15),
                    font: FONT,
                    padding: Point { h: 5,
                                     v: 13 },
                    row_height: 18,
                    count,
                    ..Default::default() }
}

impl<'a> Viewer<'a>
{
    fn open(system: &System<'a>, entry: &files::CatalogInfo) -> ToolboxResult<Self>
    {
        let mut stage = "inspect";
        let result: ToolboxResult<Self> = (|| {
            let suitcase = system.quickdraw().fonts().inspect_file(entry.volume, entry.id)?;
            stage = "layout";
            let (bounds, procedure) = layout(system)?;
            let size = Point { h: bounds.right - bounds.left,
                               v: bounds.bottom - bounds.top };
            stage = "window";
            let window = system.windows().create_owned(
                windows::WindowSpec::new(bounds, entry.name())
                    .procedure(procedure)
                    .minimum(MINIMUM.0, MINIMUM.1),
            )?;
            stage = "close-policy";
            window.set_close_deferred(true)?;
            stage = "grow-clip";
            system.windows().set_grow_icon_clip(window.id(),
                                                 Some(Rect::new(size.h - 15,
                                                                size.v - 15,
                                                                size.h,
                                                                size.v)))?;
            stage = "scrollbox";
            let list =
                system.controls().create_scroll_box(window.id(), spec(size, suitcase.count()))?;
            let viewer = Self { list,
                                suitcase,
                                identity: Location { volume: entry.volume,
                                                     folder: entry.id },
                                size,
                                window };
            stage = "draw";
            viewer.draw(system)?;
            stage = "select";
            viewer.window.select()?;
            Ok(viewer)
        })();
        if let Err(error) = &result
        {
            let mut message = crate::browser::Text::<512>::default();
            let _ = writeln!(&mut message,
                             "FINDER_FONT_OPEN_FAILED stage={stage} volume={} cnid={} status={} \
                              error={error:?} name={:?}",
                             entry.volume,
                             entry.id,
                             error.status(),
                             entry.name());
            let _ = system.log(message.as_str());
        }
        result
    }

    fn fill(&self, rows: core::ops::Range<u32>) -> ToolboxResult<()>
    {
        for row in rows
        {
            let family = self.suitcase.family(row)?;
            self.list.control().set_row(row, family.name(), false)?;
        }
        Ok(())
    }

    fn geometry_changed(&mut self, system: &System<'_>) -> ToolboxResult<()>
    {
        let bounds = self.window.info()?.bounds;
        let size = Point { h: bounds.right - bounds.left,
                           v: bounds.bottom - bounds.top };
        if size == self.size
        {
            return Ok(());
        }
        let update = self.list.control().configure(spec(size, self.suitcase.count()))?;
        self.size = size;
        system.windows().set_grow_icon_clip(self.window.id(),
                                             Some(Rect::new(size.h - 15,
                                                            size.v - 15,
                                                            size.h,
                                                            size.v)))?;
        self.fill(update.missing)
    }

    fn draw(&self, system: &System<'_>) -> ToolboxResult<()>
    {
        let list = self.list.control();
        self.fill(list.info()?.missing)?;
        let update = system.windows().update(self.window.id())?;
        let port = system.quickdraw().graphics(self.window.id());
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(Rect::new(0, 0, self.size.h, self.size.v))?;
        list.draw()?;
        update.finish()
    }

    fn key(&self, event: &Event) -> ToolboxResult<()>
    {
        if event.input().modifiers.contains(events::Modifiers::COMMAND)
        {
            return Ok(());
        }
        let list = self.list.control();
        let view = list.info()?;
        let top = match event.key().map(|key| key.code())
                  {
                      Some(0x74) => view.top.saturating_sub(view.rows()),
                      Some(0x79) => view.top.saturating_add(view.rows()),
                      Some(0x7e) => view.top.saturating_sub(1),
                      Some(0x7d) => view.top.saturating_add(1),
                      Some(0x73) => 0,
                      Some(0x77) => view.maximum(),
                      _ => return Ok(())
                  }.min(view.maximum());
        if top != view.top
        {
            self.fill(list.scroll_to(top)?.missing)?;
            list.draw()?;
        }
        Ok(())
    }
}

impl<'a> Controller<'a>
{
    pub fn open(&mut self, system: &System<'a>, entry: &files::CatalogInfo) -> ToolboxResult<()>
    {
        let identity = Location { volume: entry.volume,
                                  folder: entry.id };
        for slot in &mut self.windows
        {
            if let Some(viewer) = slot
            {
                match viewer.window.info()
                {
                    Ok(_) if viewer.identity == identity => return viewer.window.select(),
                    Ok(_) => (),
                    Err(Error::NotFound) => *slot = None,
                    Err(error) => return Err(error)
                }
            }
        }
        let slot = self.windows.iter_mut().find(|slot| slot.is_none()).ok_or(Error::NoMemory)?;
        *slot = Some(Viewer::open(system, entry)?);
        Ok(())
    }

    pub fn geometry_changed(&mut self, system: &System<'_>, window: WindowId)
                            -> ToolboxResult<bool>
    {
        let Some(slot) =
            self.windows
                .iter_mut()
                .find(|slot| slot.as_ref().is_some_and(|viewer| viewer.window.id() == window))
        else
        {
            return Ok(false);
        };
        if let Some(viewer) = slot
        {
            match viewer.geometry_changed(system)
            {
                Ok(()) => (),
                Err(Error::NotFound) if viewer.window.info() == Err(Error::NotFound) =>
                {
                    *slot = None
                }
                Err(error) => return Err(error)
            }
        }
        Ok(true)
    }

    fn close(&mut self, slot: usize) -> ToolboxResult<()>
    {
        if let Some(viewer) = &mut self.windows[slot]
        {
            viewer.window.close()?;
        }
        self.windows[slot] = None;
        Ok(())
    }

    pub fn event(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                 -> ToolboxResult<bool>
    {
        if self.windows.iter().all(Option::is_none)
        {
            return Ok(false);
        }
        if let Some(slot) =
            self.windows
                .iter()
                .position(|row| {
                    row.as_ref().is_some_and(|row| Some(row.window.id()) == event.window())
                })
        {
            if matches!(event, Event::CloseRequested { .. })
            {
                self.close(slot)?;
                return Ok(true);
            }
            if matches!(event,
                        Event::Other { kind: events::classic::EventKind::OsEvent,
                                       message: 0x0200_0000,
                                       .. })
            {
                self.windows[slot] = None;
                return Ok(true);
            }
            let viewer = self.windows[slot].as_ref().ok_or(Error::NotFound)?;
            match event
            {
                Event::Update { .. } =>
                {
                    viewer.draw(system)?;
                    return Ok(true);
                }
                Event::Activate { .. } => return Ok(true),
                Event::MouseDown { .. } =>
                {
                    let list = viewer.list.control();
                    let point = position.in_window(viewer.window.id())?.point()?;
                    if list.info()?.spec.scrollbar.contains(point)
                    {
                        list.track(point, |update| viewer.fill(update.missing))?;
                    }
                    return Ok(true);
                }
                _ => ()
            }
        }
        if !matches!(event, Event::KeyDown { .. } | Event::KeyUp { .. } | Event::MenuCommand { .. })
        {
            return Ok(false);
        }
        let front = system.windows().front()?;
        let Some(slot) =
            self.windows
                .iter()
                .position(|row| row.as_ref().is_some_and(|row| Some(row.window.id()) == front))
        else
        {
            return Ok(false);
        };
        if matches!(event, Event::MenuCommand { .. })
        {
            if system.menus().selection(event)?.is_some_and(|s| s.command == crate::menus::CLOSE)
            {
                self.close(slot)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if matches!(event, Event::KeyDown { .. })
        {
            if event.input().modifiers.contains(events::Modifiers::COMMAND)
               && event.key().is_some_and(|key| key.character().eq_ignore_ascii_case(&'w'))
            {
                self.close(slot)?;
            }
            else if let Some(viewer) = &self.windows[slot]
            {
                viewer.key(event)?;
            }
        }
        Ok(true)
    }
}
