//! One owned Multiple Items window, with bounded recursive size work.
use core::fmt::Write;

use mac_toolbox::{windows::{Window, WindowSpec},
                  *};

use super::{format::grouped, layout::FONT, totals::Job};
use crate::{browser::{Location, Text},
            selection::CAPACITY};
const STRINGS: i16 = 20030;
const LAYOUT: i16 = 20031;
pub(super) struct Summary<'a>
{
    window:     Window<'a>,
    identities: [Option<Location>; CAPACITY],
    count:      usize,
    next:       usize,
    job:        Option<Job>,
    logical:    u64,
    allocated:  Option<u64>,
    complete:   bool,
    error:      Option<Error>,
    labels:     [Text<96>; 8],
    bounds:     Rect,
    rows:       [Rect; 4]
}
impl<'a> Summary<'a>
{
    pub fn new(system: &System<'a>, identities: [Option<Location>; CAPACITY])
               -> Result<Self, Error>
    {
        let count = identities.iter().flatten().count();
        if count < 2
        {
            return Err(Error::BadArgument);
        }
        let mut labels = core::array::from_fn(|_| Text::default());
        for (index, label) in labels.iter_mut().enumerate()
        {
            let mut bytes = [0; 384];
            label.write_str(system.resources()
                                  .application()
                                  .indexed_string(STRINGS, index as u32, &mut bytes)?)
                 .map_err(|_| Error::BadArgument)?;
        }
        let resource =
            system.resources().application().get(ResourceKey { kind: TypeCode::new(*b"gSum"),
                                                                id:   LAYOUT })?;
        let mut bytes = [0; 40];
        let read = resource.read(0, &mut bytes);
        let released = resource.release();
        let read = read?;
        released?;
        if read != bytes.len() || &bytes[..4] != b"SUM1"
        {
            return Err(Error::BadArgument);
        }
        let word = |offset| i16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        let width = word(4);
        let height = word(6);
        if !(200..=600).contains(&width) || !(100..=400).contains(&height)
        {
            return Err(Error::BadArgument);
        }
        let rows = core::array::from_fn(|index| {
            let at = 8 + index * 8;
            Rect::new(word(at), word(at + 2), word(at + 4), word(at + 6))
        });
        if rows.iter().any(|row| {
                          row.is_empty()
                          || row.left < 0
                          || row.top < 0
                          || row.right > width
                          || row.bottom > height
                      })
        {
            return Err(Error::BadArgument);
        }
        let window = system.windows()
                           .create_owned(WindowSpec::new(Rect::new(8, 46, 8 + width, 46 + height),
                                                         labels[0].as_str()).minimum(width, height)
                                                                            .procedure(4))?;
        system.windows().set_grow_icon_clip(window.id(), Some(Rect::ZERO))?;
        let result = Self { window,
                            identities,
                            count,
                            next: 0,
                            job: None,
                            logical: 0,
                            allocated: Some(0),
                            complete: false,
                            error: None,
                            labels,
                            bounds: Rect::new(0, 0, width, height),
                            rows };
        result.draw(system, false)?;
        let mut log = Text::<100>::default();
        let _ =
            writeln!(&mut log, "FINDER_INFO_MULTIPLE window={} selected={}", result.id(), count);
        system.log(log.as_str())?;
        Ok(result)
    }

    pub fn id(&self) -> WindowId
    {
        self.window.id()
    }

    pub fn pending(&self) -> bool
    {
        !self.complete
    }

    fn add(&mut self, logical: u64, allocated: Option<u64>) -> Result<(), Error>
    {
        self.logical = self.logical.checked_add(logical).ok_or(Error::BadArgument)?;
        self.allocated = match (self.allocated, allocated)
        {
            (Some(total), Some(value)) =>
            {
                Some(total.checked_add(value).ok_or(Error::BadArgument)?)
            }
            _ => None
        };
        Ok(())
    }

    fn advance(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if let Some(job) = self.job.as_mut()
        {
            job.step(system)?;
            if !job.complete
            {
                return Ok(());
            }
            let logical = job.logical;
            let allocated = job.allocated;
            self.add(logical, allocated)?;
            self.job = None;
        }
        else if let Some(identity) = self.identities.get(self.next).copied().flatten()
        {
            let entry = system.files().catalog(identity.volume, identity.folder)?;
            self.next += 1;
            if entry.is_directory()
            {
                self.job = Some(Job::new(identity.volume, identity.folder));
            }
            else
            {
                let logical = entry.data_length
                                   .checked_add(entry.resource_length)
                                   .ok_or(Error::BadArgument)?;
                let allocated = match system.files()
                                            .allocation_info(identity.volume, identity.folder)
                {
                    Ok(info) => info.total_bytes()?,
                    Err(Error::NotSupported) => None,
                    Err(error) => return Err(error)
                };
                self.add(logical, allocated)?;
            }
        }
        self.complete = self.next == self.count && self.job.is_none();
        Ok(())
    }

    pub fn step(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.complete
        {
            return Ok(());
        }
        if let Err(error) = self.advance(system)
        {
            self.error = Some(error);
            self.complete = true;
        }
        if self.complete
        {
            self.draw(system, true)?;
            let mut log = Text::<160>::default();
            let _ = writeln!(&mut log,
                             "FINDER_INFO_MULTIPLE_TOTALS selected={} logical={} allocated={:?} \
                              error={:?}",
                             self.count, self.logical, self.allocated, self.error);
            system.log(log.as_str())?;
        }
        Ok(())
    }

    pub fn draw(&self, system: &System<'_>, values_only: bool) -> Result<(), Error>
    {
        let graphics = system.quickdraw().graphics(self.id());
        let update = if values_only
        {
            Some(graphics.masked_rectangles(&self.rows[1..])?)
        }
        else
        {
            system.begin_update(self.id())?;
            None
        };
        let result = (|| {
            graphics.reset()?;
            graphics.colors(0xffffff, 0xffffff)?;
            graphics.paint_rect(self.bounds)?;
            for (index, row) in self.rows.iter().copied().enumerate().skip(usize::from(values_only))
            {
                let mut value = Text::<256>::default();
                match index
                {
                    0 =>
                    {
                        write!(&mut value,
                               "{} {}",
                               grouped(self.count as u64).as_str(),
                               self.labels[1].as_str())
                    }
                    1 =>
                    {
                        if !self.complete
                        {
                            write!(&mut value,
                                   "{} {}",
                                   self.labels[2].as_str(),
                                   self.labels[4].as_str())
                        }
                        else if self.error.is_some()
                        {
                            write!(&mut value,
                                   "{} {}",
                                   self.labels[2].as_str(),
                                   self.labels[6].as_str())
                        }
                        else
                        {
                            write!(&mut value,
                                   "{} {} {}",
                                   self.labels[2].as_str(),
                                   grouped(self.logical).as_str(),
                                   self.labels[5].as_str())
                        }
                    }
                    2 =>
                    {
                        if !self.complete
                        {
                            write!(&mut value,
                                   "{} {}",
                                   self.labels[3].as_str(),
                                   self.labels[4].as_str())
                        }
                        else if let Some(allocated) =
                            self.allocated.filter(|_| self.error.is_none())
                        {
                            write!(&mut value,
                                   "{} {} {}",
                                   self.labels[3].as_str(),
                                   grouped(allocated).as_str(),
                                   self.labels[5].as_str())
                        }
                        else
                        {
                            write!(&mut value,
                                   "{} {}",
                                   self.labels[3].as_str(),
                                   self.labels[6].as_str())
                        }
                    }
                    _ =>
                    {
                        if let Some(error) = self.error
                        {
                            write!(&mut value,
                                   "{}: {:?} ({})",
                                   self.labels[7].as_str(),
                                   error,
                                   error.status())
                        }
                        else
                        {
                            Ok(())
                        }
                    }
                }.map_err(|_| Error::BadArgument)?;
                graphics.text(FONT,
                              Point { h: row.left,
                                      v: row.top + 12 },
                              value.as_str(),
                              0)?;
            }
            Ok(())
        })();
        if let Some(update) = update
        {
            result?;
            update.commit()
        }
        else
        {
            let end = system.end_update(self.id());
            result.and(end)
        }
    }
}
