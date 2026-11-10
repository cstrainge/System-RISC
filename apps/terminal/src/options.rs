//! Managed group-list options; Terminal output continues through the normal event loop.
use mac_toolbox::{Error, Event, Point, PointerPosition, Rect, System, WindowId,
                  controls::{self, Control, Reflow, ScrollBox, StandardKind, StandardState},
                  windows};

use crate::{preferences::Settings, schemes::SCHEMES};

pub(super) enum Outcome
{
    Pending,
    Cancel,
    Apply(usize)
}
pub(super) struct Options<'a>
{
    groups:   ScrollBox<'a>,
    schemes:  ScrollBox<'a>,
    cancel:   Control<'a>,
    apply:    Control<'a>,
    message:  Control<'a>,
    window:   windows::Window<'a>,
    selected: usize
}
impl<'a> Options<'a>
{
    pub fn id(&self) -> WindowId
    {
        self.window.id()
    }

    pub fn open(system: &System<'a>, settings: &Settings) -> Result<Self, Error>
    {
        let stage = std::cell::Cell::new("selection");
        let result = (|| -> Result<Self, Error> {
            let selected =
                SCHEMES.iter().position(|s| s.id == settings.scheme).ok_or(Error::CorruptData)?;
            stage.set("window");
            let window =
                system.windows()
                      .create_owned(windows::WindowSpec::new(Rect::new(140, 100, 660, 440),
                                                             "Terminal Options").procedure(1))?;
            stage.set("grow-clip");
            system.windows().set_grow_icon_clip(window.id(), Some(Rect::ZERO))?;
            let make_list = |left, right, count| {
                system.controls().create_scroll_box(window.id(),
                                                controls::ScrollBoxSpec { viewport:
                                                                              Rect::new(left,
                                                                                        16,
                                                                                        right - 16,
                                                                                        256),
                                                                          scrollbar:
                                                                              Rect::new(right - 16,
                                                                                        16,
                                                                                        right,
                                                                                        256),
                                                                          count,
                                                                          row_height: 20,
                                                                          padding: Point { h: 5,
                                                                                           v: 14 },
                                                                          ..Default::default() })
            };
            stage.set("groups");
            let groups = make_list(16, 150, 1)?;
            stage.set("scheme-list");
            let schemes = make_list(166, 504, SCHEMES.len() as u32)?;
            stage.set("cancel-button");
            let cancel =
                system.controls().create_standard(window.id(),
                                                   StandardKind::Button,
                                                   StandardState::new(Rect::new(320, 300, 400,
                                                                                324),
                                                                      "Cancel"),
                                                   Reflow::No)?;
            let mut apply_state = StandardState::new(Rect::new(416, 300, 500, 324), "Apply");
            apply_state.flags = controls::DEFAULT_BUTTON;
            stage.set("apply-button");
            let apply =
                system.controls().create_standard(window.id(),
                                                   StandardKind::Button,
                                                   apply_state,
                                                   Reflow::No)?;
            stage.set("message-control");
            let message = system.controls()
                                .create_static_text(window.id(),
                                                    Rect::new(16, 266, 504, 292),
                                                    "Applies to open and new terminals.",
                                                    controls::FontSelection::default(),
                                                    controls::TextAlignment::Left)?
                                .into_control();
            let result = Self { groups,
                                schemes,
                                cancel,
                                apply,
                                message,
                                window,
                                selected };
            stage.set("group-row");
            result.groups.control().set_row(0, "Schemes", true)?;
            stage.set("scheme-rows");
            result.fill(result.schemes.control().info()?.missing)?;
            stage.set("initial-paint");
            result.draw(system)?;
            stage.set("select-window");
            result.window.select()?;
            Ok(result)
        })();
        result.map_err(|error| {
                  let _ = system.log(&format!("TERMINAL_OPTIONS_FAILED stage={} error={error}\n",
                                              stage.get()));
                  error
              })
    }

    fn fill(&self, range: core::ops::Range<u32>) -> Result<(), Error>
    {
        let view = self.schemes.control().info()?;
        for row in range
        {
            if row < view.top || row >= view.top + view.rows()
            {
                continue;
            }
            let scheme = SCHEMES.get(row as usize).ok_or(Error::BadArgument)?;
            self.schemes.control().set_row(row, scheme.name, row as usize == self.selected)?;
        }
        Ok(())
    }

    pub fn error(&self, error: Error) -> Result<(), Error>
    {
        let text = format!("Terminal Prefs error: {error}");
        self.message.control().static_text().set(&text)?;
        self.message.control().draw()
    }

    fn draw(&self, system: &System<'a>) -> Result<(), Error>
    {
        let update = system.windows().update(self.id())?;
        let palette = mac_toolbox::quickdraw::theme::current(system.client())?;
        let port = system.quickdraw().graphics(self.id());
        port.reset()?;
        port.colors(palette.content, palette.content)?;
        port.paint_rect(update.bounds())?;
        self.groups.control().draw()?;
        self.schemes.control().draw()?;
        for control in [&self.cancel, &self.apply, &self.message]
        {
            control.control().draw()?;
        }
        update.finish()
    }

    pub fn handle(&mut self, system: &System<'a>, event: &Event, position: PointerPosition)
                  -> Result<Outcome, Error>
    {
        let front = system.windows().front()? == Some(self.id());
        match event
        {
            Event::CloseRequested { .. } if event.window() == Some(self.id()) =>
            {
                return Ok(Outcome::Cancel);
            }
            Event::Update { .. } | Event::WindowGeometryChanged { .. }
                if event.window() == Some(self.id()) =>
            {
                self.draw(system)?
            }
            Event::KeyDown { .. } if front =>
            {
                match event.key().map(|k| k.character())
                {
                    Some('\u{1b}') => return Ok(Outcome::Cancel),
                    Some('\r') => return Ok(Outcome::Apply(self.selected)),
                    _ => ()
                }
            }
            Event::MouseDown { .. } if event.window() == Some(self.id()) =>
            {
                let point = position.in_window(self.id())?;
                for (control, apply) in [(&self.cancel, false), (&self.apply, true)]
                {
                    let mut action = false;
                    if control.control().track_at(point, |state| {
                                             action |= state.action;
                                             Ok(())
                                         })?
                    {
                        return Ok(if action
                                  {
                                      if apply
                                      {
                                          Outcome::Apply(self.selected)
                                      }
                                      else
                                      {
                                          Outcome::Cancel
                                      }
                                  }
                                  else
                                  {
                                      Outcome::Pending
                                  });
                    }
                }
                let list = self.schemes.control();
                if let Some(row) = list.row_at(point.point()?)?
                {
                    if self.selected != row as usize
                    {
                        let old = self.selected as u32;
                        self.selected = row as usize;
                        self.fill(old..old + 1)?;
                        self.fill(row..row + 1)?;
                        list.draw()?;
                    }
                }
                else
                {
                    list.track(point.point()?, |view| self.fill(view.missing))?;
                }
            }
            _ => ()
        }
        Ok(Outcome::Pending)
    }
}
