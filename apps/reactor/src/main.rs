//! First inspection customer. Plain managed lists precede the shared view
//! protocol; incomplete model coverage is visible rather than fabricated.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{controls::{ScrollBox, ScrollBoxSpec, StaticText, TextAlignment, scrollbox::View},
                  inspection::{CapacityPolicy, Domain, LedgerSnapshot, Pool, Scope, Snapshot},
                  quickdraw::{fonts::FontSelection, theme},
                  *};
mac_toolbox::panic_handler!();
const QUIT: u32 = u32::from_be_bytes(*b"quit");
const REFRESH: u32 = u32::from_be_bytes(*b"rfrs");
const FONT: FontSelection = FontSelection { family: 3,
                                            size:   9,
                                            face:   0 };

struct Line
{
    bytes: [u8; 768],
    len:   usize
}
impl Line
{
    fn new() -> Self
    {
        Self { bytes: [0; 768],
               len:   0 }
    }

    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}
impl Write for Line
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let end = self.len
                      .checked_add(text.len())
                      .filter(|&end| end <= self.bytes.len())
                      .ok_or(fmt::Error)?;
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
fn label<'a>(system: &System<'_>, index: u32, bytes: &'a mut [u8]) -> ToolboxResult<&'a str>
{
    system.resources().application().indexed_string(2000, index, bytes)
}
fn inspection_error(system: &System<'_>, error: inspection::Failure) -> Error
{
    if error.cause == Error::ScanStale
    {
        return error.cause; // Normal snapshot churn is handled by the refresh path.
    }
    let mut line = Line::new();
    let _ = writeln!(line, "REACTOR_INSPECTION_ERROR: {error}");
    let _ = system.log(line.text());
    error.cause
}
fn snapshot<'a>(system: &System<'a>) -> ToolboxResult<Snapshot<'a>>
{
    system.inspection().snapshot(Scope::All).map_err(|e| inspection_error(system, e))
}

fn pool_name(line: &mut Line, kind: [u8; 4]) -> fmt::Result
{
    for byte in kind
    {
        if byte.is_ascii_graphic()
        {
            write!(line, "{}", char::from(byte))?;
        }
        else
        {
            write!(line, "\\x{byte:02x}")?;
        }
    }
    Ok(())
}

fn window_layout(system: &System<'_>) -> ToolboxResult<(Rect, i16)>
{
    let resource = system.resources().application().get(ResourceKey::from_bytes(*b"WIND", 2003))?;
    let mut bytes = [0; 20];
    if resource.size() != bytes.len()
    {
        resource.release()?;
        return Err(Error::CorruptData);
    }
    let read = resource.read(0, &mut bytes);
    resource.release()?;
    if read? != bytes.len()
    {
        return Err(Error::CorruptData);
    }
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    if bounds.right - bounds.left < 520 || bounds.bottom - bounds.top < 300
    {
        return Err(Error::CorruptData);
    }
    Ok((bounds, word(8)))
}

struct Panes<'a>
{
    domains:        ScrollBox<'a>,
    details:        ScrollBox<'a>,
    status:         StaticText<'a>,
    width:          i16,
    height:         i16,
    selected:       Option<Domain>,
    ledger:         Option<LedgerSnapshot<'a>>,
    selected_index: Option<u32>
}
fn spec(bounds: Rect, count: u32, palette: theme::Palette) -> ScrollBoxSpec
{
    ScrollBoxSpec { viewport: Rect::new(bounds.left,
                                        bounds.top,
                                        bounds.right - 16,
                                        bounds.bottom),
                    scrollbar: Rect::new(bounds.right - 16,
                                         bounds.top,
                                         bounds.right,
                                         bounds.bottom),
                    font: FONT,
                    padding: Point { h: 5,
                                     v: 12 },
                    row_height: 18,
                    count,
                    foreground: palette.text,
                    background: palette.content,
                    selected_foreground: palette.selected_text,
                    selected_background: palette.selection,
                    ..Default::default() }
}
impl<'a> Panes<'a>
{
    fn resize(&mut self, system: &System<'a>, window: WindowId, width: i16, height: i16,
              snapshot: Snapshot<'a>, palette: theme::Palette)
              -> ToolboxResult<()>
    {
        if (width, height) == (self.width, self.height)
        {
            return Ok(());
        }
        let split = width * 3 / 5;
        let count = u32::try_from(snapshot.count).map_err(|_| Error::BadAbi)?;
        let view = self.domains
                       .control()
                       .configure(spec(Rect::new(0, 26, split, height - 25), count, palette))?;
        self.details.control().configure(spec(Rect::new(split + 1, 26, width, height - 25),
                                               self.detail_count()?,
                                               palette))?;
        self.status.set_bounds(Rect::new(5, height - 22, width - 4, height - 2))?;
        self.width = width;
        self.height = height;
        system.windows().set_grow_icon_clip(window,
                                             Some(Rect::new(width - 16,
                                                            height - 16,
                                                            width,
                                                            height)))?;
        self.populate(snapshot, view)?;
        self.detail_rows(system)
    }

    fn new(system: &System<'a>, window: WindowId, width: i16, height: i16, snapshot: Snapshot<'a>,
           palette: theme::Palette)
           -> ToolboxResult<Self>
    {
        let split = width * 3 / 5;
        let domains =
            system.controls()
                  .create_scroll_box(window,
                                     spec(Rect::new(0, 26, split, height - 25),
                                          u32::try_from(snapshot.count).map_err(|_| {
                                                                           Error::BadAbi
                                                                       })?,
                                          palette))?;
        let details = system.controls().create_scroll_box(window,
                                                           spec(Rect::new(split + 1,
                                                                          26,
                                                                          width,
                                                                          height - 25),
                                                                11,
                                                                palette))?;
        let mut text = [0; 128];
        let status = system.controls().create_static_text(window,
                                                           Rect::new(5,
                                                                     height - 22,
                                                                     width - 4,
                                                                     height - 2),
                                                           label(system, 0, &mut text)?,
                                                           FONT,
                                                           TextAlignment::Left)?;
        let result = Self { domains,
                            details,
                            status,
                            width,
                            height,
                            selected: None,
                            ledger: None,
                            selected_index: None };
        result.populate(snapshot, result.domains.control().info()?)?;
        result.detail_rows(system)?;
        Ok(result)
    }

    /// Called on snapshot/view changes, never from the drawing path.
    fn populate(&self, snapshot: Snapshot<'a>, view: View) -> ToolboxResult<()>
    {
        for index in view.missing
        {
            let row = snapshot.at(u64::from(index)).map_err(|e| e.cause)?;
            self.set_domain_row(index,
                                row,
                                self.selected.is_some_and(|selected| {
                                                 selected.incarnation == row.incarnation
                                             }))?;
        }
        Ok(())
    }

    fn set_domain_row(&self, index: u32, row: Domain, selected: bool) -> ToolboxResult<()>
    {
        let mut text = Line::new();
        write!(text, "{}  #{}   {} tracked", row.name.as_str(), row.incarnation, row.live).map_err(|_| Error::BadArgument)?;
        self.domains.control().set_row(index, text.text(), selected)
    }

    fn detail_count(&self) -> ToolboxResult<u32>
    {
        u32::try_from(self.ledger.map_or(0, |ledger| ledger.len())).ok()
                                                                   .and_then(|kinds| {
                                                                       kinds.checked_mul(4)
                                                                   })
                                                                   .and_then(|rows| {
                                                                       rows.checked_add(11)
                                                                   })
                                                                   .ok_or(Error::BadAbi)
    }

    fn configure_details(&self) -> ToolboxResult<()>
    {
        let mut spec = self.details.control().info()?.spec;
        spec.count = self.detail_count()?;
        self.details.control().configure(spec)?;
        Ok(())
    }

    fn detail_rows(&self, system: &System<'_>) -> ToolboxResult<()>
    {
        let view = self.details.control().info()?;
        // At most one public read per missing pool, not four per text row.
        // Managed row caches service repaints without re-enumerating anything.
        let mut cached: Option<(u64, Pool)> = None;
        for index in view.missing
        {
            let mut line = Line::new();
            if let Some(row) = self.selected
            {
                match index
                {
                    0 => write!(line, "{}", row.name.as_str()),
                    1 => write!(line, "Incarnation: {}", row.incarnation),
                    2 => write!(line, "Ledger revision: {}", row.ledger_revision),
                    3 => write!(line, "Tracked live: {}", row.live),
                    4 => write!(line, "Births / deaths: {} / {}", row.births, row.deaths),
                    5 => write!(line, "Slots / retired: {} / {}", row.slots, row.retired),
                    6 => write!(line, "Migrated kinds: {}", row.kinds),
                    7 => write!(line, "Pending cleanup: {}", row.pending),
                    8 =>
                    {
                        write!(line,
                               "Admission: {}",
                               if row.accepting { "open" } else { "stopping" })
                    }
                    9 | 10 =>
                    {
                        let mut bytes = [0; 128];
                        line.write_str(label(system, index - 7, &mut bytes)?)
                    }
                    _ =>
                    {
                        let kind_index = u64::from((index - 11) / 4);
                        let pool = match cached
                        {
                            Some((at, pool)) if at == kind_index => pool,
                            _ =>
                            {
                                let pool = self.ledger
                                               .ok_or(Error::BadAbi)?
                                               .at(kind_index)
                                               .map_err(|e| inspection_error(system, e))?;
                                cached = Some((kind_index, pool));
                                pool
                            }
                        };
                        match (index - 11) % 4
                        {
                            0 =>
                            {
                                pool_name(&mut line, pool.kind).map_err(|_| Error::BadArgument)?;
                                write!(line, ": {} live, {} pending", pool.live, pool.pending)
                            }
                            1 =>
                            {
                                match pool.capacity
                                {
                                    CapacityPolicy::GrowableUntilMemory =>
                                    {
                                        line.write_str("Capacity: growable until memory")
                                    }
                                    CapacityPolicy::PerDomainQuota { slots } =>
                                    {
                                        write!(line, "Domain quota: {slots} slots")
                                    }
                                    CapacityPolicy::HardBound { slots,
                                                                reason } =>
                                    {
                                        write!(line, "Bound: {slots}; {}", reason.as_str())
                                    }
                                }
                            }
                            2 => write!(line, "Gen. headroom: {}", pool.generation_headroom),
                            _ =>
                            {
                                write!(line,
                                       "Slots {} / retired {}; births {} / deaths {}",
                                       pool.slots, pool.retired, pool.births, pool.deaths)
                            }
                        }
                    }
                }.map_err(|_| Error::BadArgument)?;
            }
            else if index == 0
            {
                let mut bytes = [0; 128];
                line.write_str(label(system, 1, &mut bytes)?).map_err(|_| Error::BadArgument)?;
            }
            self.details.control().set_row(index, line.text(), false)?;
        }
        Ok(())
    }

    fn refresh(&mut self, system: &System<'a>, snapshot: Snapshot<'a>, paint: bool)
               -> ToolboxResult<()>
    {
        let mut config = self.domains.control().info()?.spec;
        config.count = u32::try_from(snapshot.count).map_err(|_| Error::BadAbi)?;
        self.domains.control().configure(config)?;
        let view = self.domains.control().invalidate()?;
        self.populate(snapshot, view)?;
        // Selected identity is stable across membership changes; locate it only
        // on a new snapshot, never for every click or frame.
        if let Some(selected) = self.selected
        {
            self.selected = None;
            self.ledger = None;
            self.selected_index = None;
            for index in 0..snapshot.count
            {
                let ledger = snapshot.ledger(index).map_err(|e| inspection_error(system, e))?;
                let row = ledger.domain();
                if row.incarnation == selected.incarnation
                {
                    self.selected = Some(row);
                    self.ledger = Some(ledger);
                    self.selected_index = Some(index as u32);
                    break;
                }
            }
            self.configure_details()?;
            self.details.control().invalidate()?;
            self.detail_rows(system)?;
        }
        if paint
        {
            self.domains.control().draw()?;
            self.details.control().draw()?;
        }
        Ok(())
    }

    fn click(&mut self, system: &System<'a>, snapshot: Snapshot<'a>, point: Point)
             -> ToolboxResult<()>
    {
        if let Some(index) = self.domains.control().row_at(point)?
        {
            if self.selected_index == Some(index)
            {
                return Ok(());
            }
            let ledger =
                snapshot.ledger(u64::from(index)).map_err(|e| inspection_error(system, e))?;
            let row = ledger.domain();
            if let (Some(previous), Some(previous_index)) = (self.selected, self.selected_index)
            {
                let view = self.domains.control().info()?;
                if previous_index >= view.top && previous_index < view.top + view.rows()
                {
                    self.set_domain_row(previous_index, previous, false)?;
                }
            }
            self.selected = Some(row);
            self.ledger = Some(ledger);
            self.selected_index = Some(index);
            self.set_domain_row(index, row, true)?;
            self.configure_details()?;
            self.details.control().invalidate()?;
            self.detail_rows(system)?;
            self.domains.control().draw()?;
            self.details.control().draw()?;
            return Ok(());
        }
        self.domains.control().track(point, |view| self.populate(snapshot, view))?;
        self.details.control().track(point, |_| self.detail_rows(system))
    }

    fn draw(&self, system: &System<'_>, window: WindowId, palette: theme::Palette)
            -> ToolboxResult<()>
    {
        let update = system.windows().update(window)?;
        let damage = update.bounds();
        let port = system.quickdraw().graphics(window);
        port.reset()?;
        port.clip_rect(damage)?;
        port.colors(palette.content, palette.content)?;
        port.paint_rect(Rect::new(0, 0, self.width, self.height))?;
        let mut text = [0; 128];
        if damage.top < 26
        {
            port.text(FONT,
                      Point { h: 6,
                              v: 17 },
                      label(system, 4, &mut text)?,
                      palette.text)?;
            port.text(FONT,
                      Point { h: self.width * 3 / 5 + 6,
                              v: 17 },
                      label(system, 5, &mut text)?,
                      palette.text)?;
        }
        if damage.bottom > 26 && damage.top < self.height - 25
        {
            if damage.left < self.width * 3 / 5
            {
                self.domains.control().draw()?;
            }
            if damage.right > self.width * 3 / 5
            {
                self.details.control().draw()?;
            }
        }
        if damage.bottom > self.height - 25
        {
            self.status.draw()?;
        }
        update.finish()
    }
}

fn run(system: &System<'_>) -> ToolboxResult<()>
{
    system.menus().clear()?;
    let _file = system.menus().install_owned(resources::APPLICATION, 128)?;
    let _view = system.menus().install_owned(resources::APPLICATION, 129)?;
    let mut title = [0; 128];
    let title = system.resources().application().string(128, &mut title)?;
    let (bounds, procedure) = window_layout(system)?;
    let mut owner =
        system.windows()
              .create_owned(windows::WindowSpec::new(bounds, title).minimum(520, 300)
                                                                   .procedure(procedure))?;
    let window = owner.id();
    owner.set_close_deferred(true)?;
    system.windows().set_grow_icon_clip(window,
                                         Some(Rect::new(bounds.right - bounds.left - 16,
                                                        bounds.bottom - bounds.top - 16,
                                                        bounds.right - bounds.left,
                                                        bounds.bottom - bounds.top)))?;
    let palette = theme::current(system.client())?;
    let mut current = snapshot(system)?;
    let mut panes = Panes::new(system,
                               window,
                               bounds.right - bounds.left,
                               bounds.bottom - bounds.top,
                               current,
                               palette)?;
    panes.draw(system, window, palette)?;
    system.log("REACTOR_READY coverage=process-domains,partial-ledgers authority=system-policy\n")?;
    let mut front = true;
    let mut dirty = false;
    let mut stale = false;
    loop
    {
        // Background waits exclude advisory OsEvents. Membership bursts coalesce
        // in System, and activation refreshes once; there is no sampling timer.
        let mask = if front
        {
            events::EventMask::ALL
        }
        else
        {
            events::EventMask::from_bits_retain(events::EventMask::ALL.bits()
                                                & !events::EventMask::SYSTEM.bits())
        };
        let (event, position) = system.wait_positioned(mask, u32::MAX)?.into_parts();
        if let Some(selection) = system.menus().selection(&event)?
        {
            match selection.command
            {
                QUIT =>
                {
                    owner.close()?;
                    return Ok(());
                }
                REFRESH => dirty = true,
                _ =>
                {}
            }
        }
        match event
        {
            Event::Activate { window: target,
                              active,
                              .. } if target == window =>
            {
                front = active;
                dirty |= active;
            }
            Event::CloseRequested { window: target,
                                    .. } if target == window =>
            {
                owner.close()?;
                return Ok(());
            }
            Event::Other { message: inspection::CHANGED_EVENT,
                           .. } => dirty = true,
            Event::WindowGeometryChanged { window: target,
                                           .. } if target == window =>
            {
                let bounds = system.windows().info(window)?.bounds;
                let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
                if (width, height) != (panes.width, panes.height)
                {
                    if let Err(error) =
                        panes.resize(system, window, width, height, current, palette)
                    {
                        if error == Error::ScanStale
                        {
                            dirty = true;
                        }
                        else
                        {
                            return Err(error);
                        }
                    }
                }
            }
            Event::Update { window: target,
                            .. } if target == window => panes.draw(system, window, palette)?,
            Event::MouseDown { window: Some(target),
                               .. } if target == window =>
            {
                if let Err(error) =
                    panes.click(system, current, position.in_window(window)?.point()?)
                {
                    if error == Error::ScanStale
                    {
                        dirty = true;
                    }
                    else
                    {
                        return Err(error);
                    }
                }
            }
            _ =>
            {}
        }
        if front && dirty
        {
            current = snapshot(system)?;
            match panes.refresh(system, current, true)
            {
                Ok(()) =>
                {
                    dirty = false;
                    if stale
                    {
                        let mut text = [0; 128];
                        panes.status.set(label(system, 0, &mut text)?)?;
                        panes.status.draw()?;
                        stale = false;
                    }
                }
                Err(Error::ScanStale) =>
                {
                    // Subject churn invalidates snapshots, not the inspector.
                    // Retry on the next event instead of a busy loop.
                    if !stale
                    {
                        let mut text = [0; 128];
                        panes.status.set(label(system, 6, &mut text)?)?;
                        panes.status.draw()?;
                        stale = true;
                    }
                }
                Err(error) => return Err(error)
            }
        }
    }
}

/// # Safety
/// The native loader supplies initialized segments and a live service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> abi::Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    run(&system).map_or_else(|error| {
                                 let mut message = Line::new();
                                 let _ = writeln!(message,
                                                  "REACTOR_ERROR: Toolbox operation failed: \
                                                   {error}");
                                 let _ = system.log(message.text());
                                 error.status()
                             },
                             |_| abi::OK)
}
