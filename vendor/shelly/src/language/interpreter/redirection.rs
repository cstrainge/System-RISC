
use std::{ fs::File,
           io::{ self, Error, ErrorKind, PipeWriter, Read, copy, pipe },
           process::{ Command, Stdio },
           rc::Rc,
           thread::{ Builder, JoinHandle } };

use crate::language::text::location::Location;


pub(super) enum Output
{
    File(File),
    Pipe(PipeWriter)
}


impl Output
{
    pub(super) fn stdio(&self) -> io::Result<Stdio>
    {
        match self
        {
            Self::File(file) => Ok(file.try_clone()?.into()),
            Self::Pipe(writer) => Ok(writer.try_clone()?.into()),
        }
    }

    pub fn copy_from(&self, input: &mut impl Read) -> io::Result<u64>
    {
        match self
        {
            Self::File(file) => copy(input, &mut &*file),
            Self::Pipe(writer) => copy(input, &mut &*writer),
        }
    }
}


pub(super) struct Redirection
{
    pub location: Location,
    pub output: Option<Rc<Output>>,
    pub error: Option<Rc<Output>>,
    capture: Option<(String, JoinHandle<io::Result<Vec<u8>>>)>,
}


impl Redirection
{
    // A worker borrows the endpoints; the originating scope alone owns captures.
    pub(super) fn borrowed(&self) -> Self
    {
        Self { location: self.location.clone(), output: self.output.clone(),
            error: self.error.clone(), capture: None }
    }

    pub fn new(location: Location) -> Self
    {
        Self { location, output: None, error: None, capture: None }
    }

    pub fn capture(&mut self, name: String) -> io::Result<Rc<Output>>
    {
        let (mut reader, writer) = pipe()?;
        let worker = Builder::new().name("shelly-output".to_string()).spawn(move ||
            {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes)?;
                Ok(bytes)
            })?;
        self.capture = Some((name, worker));
        Ok(Rc::new(Output::Pipe(writer)))
    }

    pub fn finish(self) -> io::Result<Option<(String, String)>>
    {
        let Self { output, error, capture, .. } = self;
        // Close all shell-owned write ends before joining the capture worker.
        drop((output, error));
        capture.map(|(name, worker)|
            {
                let bytes = worker.join()
                    .map_err(|_| Error::other("Capture worker panicked"))??;
                let text = String::from_utf8(bytes)
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
                Ok((name, text))
            }).transpose()
    }
}


pub(super) fn configure(command: &mut Command, redirects: &[Redirection]) -> io::Result<bool>
{
    if let Some(output) = crate::runtime::readline::output::data_stdio()?
    { command.stdout(output); }
    let output = redirects.iter().rev().find_map(|redirect| redirect.output.as_ref());
    if let Some(output) = output { command.stdout(output.stdio()?); }
    if let Some(error) = redirects.iter().rev().find_map(|redirect| redirect.error.as_ref())
    { command.stderr(error.stdio()?); }
    Ok(output.is_some())
}
