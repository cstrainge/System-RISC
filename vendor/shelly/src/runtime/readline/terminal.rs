
use crate::runtime::readline::Fault;

pub(crate) struct TerminalGuard
{
    #[cfg(unix)]
    original: libc::termios,
    restored: bool,
    protocols: std::cell::Cell<bool>,
    output: std::cell::RefCell<Option<super::output::OutputGuard>>,
}
impl TerminalGuard
{
    pub(crate) fn new() -> Result<Self, Fault>
    {
        #[cfg(unix)]
        {
            let input = libc::STDIN_FILENO;
            let output = libc::STDERR_FILENO;
            let mut input_stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let mut output_stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
            unsafe {
                if libc::fstat(input, input_stat.as_mut_ptr()) != 0
                    || libc::fstat(output, output_stat.as_mut_ptr()) != 0
                    || input_stat.assume_init().st_rdev != output_stat.assume_init().st_rdev
                {
                    return Err(Fault::new(
                        "TerminalError",
                        "NotTerminal: input/output terminal differ",
                    ));
                }
                if libc::tcgetpgrp(input) != libc::getpgrp()
                {
                    return Err(Fault::new(
                        "TerminalError",
                        "NotForeground: terminal is not foreground",
                    ));
                }
                if libc::tcgetattr(input, original.as_mut_ptr()) != 0
                {
                    return Err(Fault::io("TerminalError", std::io::Error::last_os_error()));
                }
                Ok(Self {
                    original: original.assume_init(),
                    restored: false,
                    protocols: std::cell::Cell::new(false),
                    output: std::cell::RefCell::new(None),
                })
            }
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                restored: false,
                protocols: std::cell::Cell::new(false),
                output: std::cell::RefCell::new(None),
            })
        }
    }
    pub(crate) fn enable_protocols(&self) -> Result<(), Fault>
    {
        *self.output.borrow_mut() = Some(
            super::output::OutputGuard::enter()
                .map_err(|error| Fault::io("TerminalError", error))?,
        );
        self.protocols.set(true);
        crossterm::execute!(
            std::io::stderr(),
            crossterm::event::EnableBracketedPaste,
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )
        .map_err(|error| Fault::io("TerminalError", error))
    }
    fn finish_protocols(&self)
    {
        if self.protocols.replace(false)
        {
            let _ = crossterm::execute!(
                std::io::stderr(),
                crossterm::event::PopKeyboardEnhancementFlags,
                crossterm::event::DisableBracketedPaste,
                crossterm::cursor::Show
            );
        }
    }
    pub(crate) fn restore(mut self) -> Result<(), Fault>
    {
        self.finish_protocols();
        if let Some(output) = self.output.borrow_mut().take()
        {
            output
                .restore()
                .map_err(|error| Fault::new("TerminalError", format!("Restore: {}", error)))?;
        }
        #[cfg(unix)]
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) } != 0
        {
            return Err(Fault::new(
                "TerminalError",
                format!("Restore: {}", std::io::Error::last_os_error()),
            ));
        }
        self.restored = true;
        Ok(())
    }
}
impl Drop for TerminalGuard
{
    fn drop(&mut self)
    {
        if !self.restored
        {
            self.finish_protocols();
            self.output.borrow_mut().take();
            #[cfg(unix)]
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
            }
            #[cfg(not(unix))]
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}
