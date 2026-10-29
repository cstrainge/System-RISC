//! Resource-based Finder transfer presentation. The transfer owner keeps this
//! window alive through rollback acknowledgement, not merely a Stop request.
use core::fmt::Write;

use mac_toolbox::{controls::{Control, Progress, ProgressBar, StandardKind, StandardState,
                             StaticText, TextAlignment},
                  *};

use crate::browser::Text;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation
{
    Copy,
    Move
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase
{
    Preparing,
    Copying,
    Reading,
    Writing,
    Verifying,
    Finishing,
    Undoing
}
pub struct Snapshot<'n>
{
    pub phase:       Phase,
    pub item:        &'n str,
    pub remaining:   u64,
    pub completed:   u64,
    pub in_progress: u64,
    pub total:       Option<u64>
}
struct Layout
{
    bounds:    Rect,
    origin:    Point,
    procedure: i16,
    font:      quickdraw::fonts::FontSelection,
    rects:     [Rect; 6],
    stop:      Text<16>
}
impl Layout
{
    fn load(system: &System<'_>) -> ToolboxResult<Self>
    {
        let resource =
            system.resources().application().get(ResourceKey { kind: TypeCode::new(*b"cpUI"),
                                                                id:   8751 })?;
        let mut bytes = [0; 76];
        let read = resource.read(0, &mut bytes);
        let released = resource.release();
        if read? != bytes.len()
        {
            return Err(Error::CorruptData);
        }
        released?;
        let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
        if &bytes[..4] != b"CpW1"
           || word(10) < 0
           || word(18) < 21
           || bytes[68] > 7
           || bytes[69 + bytes[68] as usize..].iter().any(|b| *b != 0)
        {
            return Err(Error::CorruptData);
        }
        let mut stop = Text::default();
        stop.write_str(core::str::from_utf8(&bytes[69..69 + bytes[68] as usize]).map_err(|_| Error::CorruptData)?)
            .map_err(|_| Error::CorruptData)?;
        let bounds = Rect::new(0, 0, word(4), word(6));
        if bounds.is_empty()
        {
            return Err(Error::CorruptData);
        }
        let mut rects = [Rect::ZERO; 6];
        for (index, r) in rects.iter_mut().enumerate()
        {
            let at = 20 + index * 8;
            *r = Rect::new(word(at + 2), word(at), word(at + 6), word(at + 4));
            if r.is_empty()
               || r.left < 0
               || r.top < 0
               || r.right > bounds.right
               || r.bottom > bounds.bottom
            {
                return Err(Error::CorruptData);
            }
        }
        Ok(Self { bounds,
                  origin: Point { h: word(10),
                                  v: word(18) },
                  procedure: word(8),
                  font: quickdraw::fonts::FontSelection { family: word(12),
                                                          size:   word(14),
                                                          face:   word(16) as u16 },
                  rects,
                  stop })
    }
}
pub struct CopyWindow<'a>
{
    label:               StaticText<'a>,
    remaining:           StaticText<'a>,
    operation_label:     StaticText<'a>,
    item:                StaticText<'a>,
    progress:            ProgressBar<'a>,
    stop:                Control<'a>,
    stop_state:          StandardState,
    layout:              Layout,
    operation:           Operation,
    stopping:            bool,
    phase:               Phase,
    displayed_remaining: Option<u64>,
    full_name:           Text<256>,
    // Controls are disposed before their parent window.
    window:              windows::Window<'a>
}
fn string(system: &System<'_>, index: u32) -> ToolboxResult<Text<256>>
{
    let mut bytes = [0; 256];
    let value = system.resources().application().indexed_string(8750, index, &mut bytes)?;
    let mut out = Text::default();
    out.write_str(value).map_err(|_| Error::BadArgument)?;
    Ok(out)
}
impl<'a> CopyWindow<'a>
{
    pub fn new(system: &System<'a>, operation: Operation) -> ToolboxResult<Self>
    {
        let layout = Layout::load(system)?;
        let mut title = [0; 32];
        let title =
            system.resources()
                  .application()
                  .indexed_string(8752, u32::from(operation == Operation::Move), &mut title)?;
        let window =
            system.windows()
                  .create_owned(windows::WindowSpec::new(Rect::new(layout.origin.h,
                                                                   layout.origin.v,
                                                                   layout.origin.h
                                                                   + layout.bounds.right,
                                                                   layout.origin.v
                                                                   + layout.bounds.bottom),
                                                         title).procedure(layout.procedure)
                                                               .minimum(layout.bounds.right,
                                                                        layout.bounds.bottom))?;
        window.set_close_deferred(true)?;
        system.windows().set_grow_icon_clip(window.id(), Some(Rect::ZERO))?;
        let controls = system.controls();
        let preparing = string(system, if operation == Operation::Copy { 10 } else { 11 })?;
        let label = controls.create_static_text(window.id(),
                                                layout.rects[0],
                                                preparing.as_str(),
                                                layout.font,
                                                TextAlignment::Left)?;
        let remaining = controls.create_static_text(window.id(),
                                                    layout.rects[1],
                                                    "",
                                                    layout.font,
                                                    TextAlignment::Right)?;
        let operation_label = controls.create_static_text(window.id(),
                                                          layout.rects[2],
                                                          "",
                                                          layout.font,
                                                          TextAlignment::Left)?;
        let item = controls.create_static_text(window.id(),
                                               layout.rects[3],
                                               "",
                                               layout.font,
                                               TextAlignment::Left)?;
        let progress =
            controls.create_progress_bar(window.id(), layout.rects[4], Progress::Preparing)?;
        let stop_state = StandardState::new(layout.rects[5], layout.stop.as_str());
        let stop = controls.create_standard(window.id(),
                                            StandardKind::Button,
                                            stop_state,
                                            mac_toolbox::controls::Reflow::No)?;
        let ui = Self { label,
                        remaining,
                        operation_label,
                        item,
                        progress,
                        stop,
                        stop_state,
                        layout,
                        operation,
                        stopping: false,
                        phase: Phase::Preparing,
                        displayed_remaining: None,
                        full_name: Text::default(),
                        window };
        ui.draw(system)?;
        Ok(ui)
    }

    pub fn id(&self) -> WindowId
    {
        self.window.id()
    }

    pub fn stopping(&self) -> bool
    {
        self.stopping
    }

    pub fn request_stop(&mut self, system: &System<'_>) -> ToolboxResult<()>
    {
        if self.stopping
        {
            return Ok(());
        }
        // Latch intent before any fallible presentation work. The caller must
        // drain its transaction even if this label/control update fails.
        self.stopping = true;
        self.stop_state.enabled = 0;
        self.stop.control().configure(StandardKind::Button, self.stop_state)?;
        self.label.set(string(system, 25)?.as_str())?;
        self.remaining.set("")?;
        Ok(())
    }

    pub fn update(&mut self, system: &System<'_>, snapshot: Snapshot<'_>) -> ToolboxResult<()>
    {
        if snapshot.phase == Phase::Undoing
        {
            return self.request_stop(system);
        }
        if self.stopping
        {
            return Ok(());
        }
        let copy = self.operation == Operation::Copy;
        let index = match snapshot.phase
        {
            Phase::Preparing =>
            {
                if copy
                {
                    10
                }
                else
                {
                    11
                }
            }
            Phase::Finishing =>
            {
                if copy
                {
                    14
                }
                else
                {
                    15
                }
            }
            _ =>
            {
                if copy
                {
                    12
                }
                else
                {
                    13
                }
            }
        };
        if self.phase != snapshot.phase
        {
            self.label.set(string(system, index)?.as_str())?;
            let operation = match snapshot.phase
            {
                Phase::Reading => Some(0),
                Phase::Writing => Some(3),
                Phase::Verifying => Some(4),
                Phase::Copying => Some(if copy { 1 } else { 2 }),
                _ => None
            };
            self.operation_label.set(if let Some(index) = operation
                                      {
                                          string(system, index)?
                                      }
                                      else
                                      {
                                          Text::default()
                                      }.as_str())?;
            self.phase = snapshot.phase;
        }
        let count = (!matches!(snapshot.phase, Phase::Preparing | Phase::Finishing)).then_some(snapshot.remaining);
        if self.displayed_remaining != count
        {
            let mut remaining = Text::<32>::default();
            if let Some(count) = count
            {
                write!(&mut remaining, "{count}").map_err(|_| Error::BadArgument)?;
            }
            self.remaining.set(remaining.as_str())?;
            self.displayed_remaining = count;
        }
        if self.full_name.as_str() != snapshot.item
        {
            let mut full = Text::default();
            full.write_str(snapshot.item).map_err(|_| Error::BadArgument)?;
            let display = fit_name(system, self.layout.font, self.layout.rects[3], snapshot.item)?;
            self.item.set(display.as_str())?;
            self.full_name = full;
        }
        self.progress.set(match snapshot.total
                          {
                              Some(total) =>
                              {
                                  Progress::Working { completed: snapshot.completed,
                                                      in_progress: snapshot.in_progress,
                                                      total }
                              }
                              None => Progress::Preparing
                          })
    }

    pub fn draw(&self, system: &System<'_>) -> ToolboxResult<()>
    {
        let update = system.windows().update(self.id())?;
        let port = system.quickdraw().graphics(self.id());
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(self.layout.bounds)?;
        self.label.draw()?;
        self.remaining.draw()?;
        self.operation_label.draw()?;
        self.item.draw()?;
        self.progress.draw()?;
        self.stop.control().draw()?;
        update.finish()
    }

    /// Standard Control Manager tracking owns hit tests, pressed feedback and
    /// cancellation; no Finder-local button tracker or screen subtraction.
    pub fn event(&mut self, system: &System<'_>, event: &Event<'_>,
                 position: events::PointerPosition)
                 -> ToolboxResult<bool>
    {
        match event
        {
            Event::KeyDown { key,
                             input,
                             .. } if key.character() == '\u{1b}'
                    || (key.character() == '.'
                        && input.modifiers.contains(events::Modifiers::COMMAND)) =>
            {
                self.request_stop(system)?;
                Ok(true)
            }
            Event::MouseDown { window: Some(id),
                               .. } if *id == self.id() =>
            {
                if !self.stopping
                {
                    let mut clicked = false;
                    self.stop.control().track_at(position.in_window(*id)?, |_| {
                                            clicked = true;
                                            Ok(())
                                        })?;
                    if clicked
                    {
                        self.request_stop(system)?;
                    }
                }
                Ok(true)
            }
            Event::Update { window,
                            .. } if *window == self.id() =>
            {
                self.draw(system)?;
                Ok(true)
            }
            Event::CloseRequested { window,
                                    .. } if *window == self.id() =>
            {
                self.request_stop(system)?;
                Ok(true)
            }
            Event::Activate { window,
                              .. } if *window == self.id() => Ok(true),
            _ => Ok(false)
        }
    }
}
fn fit_name(system: &System<'_>, font: quickdraw::fonts::FontSelection, bounds: Rect, name: &str)
            -> ToolboxResult<Text<256>>
{
    let fonts = system.quickdraw().fonts();
    let width = bounds.right - bounds.left;
    let mut out = Text::default();
    if fonts.text_width(font, name)? <= width
    {
        out.write_str(name).map_err(|_| Error::BadArgument)?;
        return Ok(out);
    }
    // Measure only a new filename, never on a chunk, poll or repaint. Keep both
    // ends so the file extension remains identifiable. All cuts preserve UTF-8.
    let mut boundaries = [0usize; 257];
    let mut count = 0;
    for (offset, _) in name.char_indices()
    {
        if count == 256
        {
            return Err(Error::BadArgument);
        }
        boundaries[count] = offset;
        count += 1;
    }
    boundaries[count] = name.len();
    let mut low = 0;
    let mut high = count;
    while low < high
    {
        let keep = (low + high).div_ceil(2);
        let mut candidate = Text::<256>::default();
        if write!(&mut candidate,
                  "{}…{}",
                  &name[..boundaries[keep.div_ceil(2)]],
                  &name[boundaries[count - keep / 2]..]).is_err()
        {
            high = keep - 1;
            continue;
        }
        if fonts.text_width(font, candidate.as_str())? <= width
        {
            low = keep;
        }
        else
        {
            high = keep - 1;
        }
    }
    write!(&mut out,
           "{}…{}",
           &name[..boundaries[low.div_ceil(2)]],
           &name[boundaries[count - low / 2]..]).map_err(|_| Error::BadArgument)?;
    Ok(out)
}
