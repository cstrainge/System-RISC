//! System's real mappings through the public inspection SDK. Managed controls
//! retain visible rows; no polling timer or application-local meter exists.
use inspection::{RegionBatch, RegionKind, SystemMemory};

use super::*;

pub(super) struct Memory<'a>
{
    // The application retains the owning window until this view is dropped.
    rows:     ScrollBox<'a>,
    header:   StaticText<'a>,
    status:   StaticText<'a>,
    window:   WindowId,
    snapshot: SystemMemory<'a>,
    cache:    Cell<Option<RegionBatch>>,
    width:    i16,
    height:   i16
}
impl<'a> Memory<'a>
{
    pub fn id(&self) -> WindowId
    {
        self.window
    }

    fn text(system: &System<'_>, index: u32, bytes: &mut [u8]) -> ToolboxResult<Line>
    {
        let mut line = Line::new();
        let _ = write!(line, "{}", label(system, index, bytes)?);
        Ok(line)
    }

    pub fn open(system: &System<'a>, window: WindowId, width: i16, height: i16,
                palette: theme::Palette)
                -> ToolboxResult<Self>
    {
        let snapshot =
            system.inspection().system_memory().map_err(|e| inspection_error(system, e))?;
        let rows = system.controls().create_scroll_box(window,spec(Rect::new(0,56,width,height-42),
            u32::try_from(snapshot.summary().region_count).map_err(|_| Error::BadAbi)?,palette))?;
        let mut text = [0; 192];
        let header = system.controls().create_static_text(window,
                                                           Rect::new(5, 2, width - 5, 52),
                                                           "",
                                                           FONT,
                                                           TextAlignment::Left)?;
        let status = system.controls()
                           .create_static_text(window,
                                               Rect::new(5, height - 40, width - 20, height - 2),
                                               Self::text(system, 7, &mut text)?.text(),
                                               FONT,
                                               TextAlignment::Left)?;
        let mut result = Self { rows,
                                header,
                                status,
                                window,
                                snapshot,
                                cache: Cell::new(None),
                                width,
                                height };
        result.summary()?;
        result.populate(result.rows.control().info()?)?;
        system.log("REACTOR_MEMORY_READY scope=shared-supervisor source=owned-page-tables \
                    accounting=unavailable\n")?;
        Ok(result)
    }

    fn summary(&mut self) -> ToolboxResult<()>
    {
        let s = self.snapshot.summary();
        let mut line = Line::new();
        let _ = write!(line,
                       "System heap: {} / {} KiB    RAM: {} MiB\nFree pool: {} MiB    Root: \
                        {:#x}\nFoundation ordinary heap: {} KiB; tracked raw CPU backing: {} KiB",
                       s.heap_used / 1024,
                       s.heap_capacity / 1024,
                       s.ram_bytes / 1048576,
                       s.free_pages * s.page_bytes / 1048576,
                       s.root_physical,
                       s.ordinary_foundation_heap_bytes / 1024,
                       s.tracked_foundation_physical_bytes / 1024);
        self.header.set(line.text())
    }

    fn populate(&self, view: View) -> ToolboxResult<()>
    {
        for index in view.missing
        {
            let index = u64::from(index);
            let row = match self.cache.get().and_then(|batch| batch.at(index))
            {
                Some(row) => row,
                None =>
                {
                    let batch = self.snapshot.batch(index).map_err(|e| e.cause)?;
                    let row = batch.at(index).ok_or(Error::BadAbi)?;
                    self.cache.set(Some(batch));
                    row
                }
            };
            let mut line = Line::new();
            let name = match row.kind
            {
                RegionKind::Device => "Device",
                RegionKind::TrustedApplicationArena => "Trusted app arena",
                RegionKind::TrustedModuleArena => "Trusted module arena",
                RegionKind::SystemCode => "System code",
                RegionKind::SystemReadOnly => "System read-only",
                RegionKind::SystemData => "System data",
                RegionKind::SystemHeap => "System heap",
                RegionKind::SystemStack => "System stack",
                RegionKind::Guard => "Unmapped stack guard",
                RegionKind::DirectMap => "RAM alias (not extra charge)",
                RegionKind::Other => "Other mapping"
            };
            let p = row.protection;
            let _ = write!(line,
                           "{:016x}  {:>8} KiB  {}{}{}  {}",
                           row.virtual_base,
                           row.bytes / 1024,
                           if p.read { 'r' } else { '-' },
                           if p.write { 'w' } else { '-' },
                           if p.execute { 'x' } else { '-' },
                           name);
            self.rows.control().set_row(index as u32, line.text(), false)?;
        }
        Ok(())
    }

    pub fn refresh(&mut self, system: &System<'a>, palette: theme::Palette) -> ToolboxResult<()>
    {
        let next = system.inspection().system_memory().map_err(|e| inspection_error(system, e))?;
        let before = self.snapshot.summary();
        let after = next.summary();
        self.snapshot = next;
        if before.revision != after.revision
        {
            self.cache.set(None);
            self.rows
                .control()
                .configure(spec(Rect::new(0, 56, self.width, self.height - 42),
                                u32::try_from(after.region_count).map_err(|_| Error::BadAbi)?,
                                palette))?;
            self.rows.control().invalidate()?;
            self.populate(self.rows.control().info()?)?;
            self.rows.control().draw()?;
        }
        if (before.heap_used,
            before.heap_capacity,
            before.free_pages,
            before.ordinary_foundation_heap_bytes,
            before.tracked_foundation_physical_bytes)
           != (after.heap_used,
               after.heap_capacity,
               after.free_pages,
               after.ordinary_foundation_heap_bytes,
               after.tracked_foundation_physical_bytes)
        {
            self.summary()?;
            self.header.draw()?;
        }
        Ok(())
    }

    pub fn event(&mut self, system: &System<'a>, event: &Event, position: events::PointerPosition,
                 palette: theme::Palette)
                 -> ToolboxResult<()>
    {
        match *event
        {
            Event::MouseDown { window: Some(window),
                               .. } if window == self.id() =>
            {
                self.rows
                    .control()
                    .track(position.in_window(window)?.point()?, |view| self.populate(view))?;
            }
            Event::WindowGeometryChanged { window,
                                           .. } if window == self.id() =>
            {
                let b = system.windows().info(self.window)?.bounds;
                let size = (b.right - b.left, b.bottom - b.top);
                if size != (self.width, self.height)
                {
                    (self.width, self.height) = size;
                    self.header.set_bounds(Rect::new(5, 2, self.width - 5, 52))?;
                    self.status.set_bounds(Rect::new(5,
                                                      self.height - 40,
                                                      self.width - 5,
                                                      self.height - 2))?;
                    let view = self.rows.control().configure(spec(Rect::new(0,56,self.width,self.height-42),
                        u32::try_from(self.snapshot.summary().region_count).map_err(|_| Error::BadAbi)?,palette))?;
                    self.populate(view)?;
                }
            }
            Event::Update { window,
                            .. } if window == self.id() =>
            {
                let update = system.windows().update(window)?;
                let damage = update.bounds();
                let port = system.quickdraw().graphics(window);
                port.reset()?;
                port.clip_rect(damage)?;
                port.colors(palette.content, palette.content)?;
                port.paint_rect(damage)?;
                if damage.top < 56
                {
                    self.header.draw()?;
                }
                if damage.bottom > 56 && damage.top < self.height - 42
                {
                    self.rows.control().draw()?;
                }
                if damage.bottom > self.height - 42
                {
                    self.status.draw()?;
                }
                update.finish()?;
            }
            _ =>
            {}
        }
        Ok(())
    }
}
