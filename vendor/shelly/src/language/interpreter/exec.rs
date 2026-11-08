
use std::{ env::split_paths,
           ffi::CString,
           io::{ self, Error, ErrorKind, Write, stderr, stdout },
           mem::zeroed,
           os::{ fd::{ AsRawFd, FromRawFd, OwnedFd, RawFd }, unix::ffi::OsStrExt },
           path::PathBuf,
           ptr::{ null, null_mut } };

use libc::{ close, dup2, execve, fcntl, sigaction, sigemptyset, EBADF, EACCES, ENOENT,
            ENOTDIR, F_DUPFD_CLOEXEC, F_GETFD, F_SETFD, SIGPIPE, SIG_DFL };

use crate::language::{ data::value::{ ExecResult, Value },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult,
                                      redirection::Output },
                       text::location::Location };


// Exec failures must leave the calling shell's descriptors and signal policy intact.
struct SavedOutput
{
    target: RawFd,
    original: Option<OwnedFd>,
    flags: i32,
    restore: bool,
}


fn duplicate(source: RawFd, target: RawFd) -> io::Result<()>
{
    loop
    {
        if unsafe { dup2(source, target) } != -1 { return Ok(()); }
        let error = Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted { return Err(error); }
    }
}


impl SavedOutput
{
    fn install(target: RawFd, source: RawFd) -> io::Result<Self>
    {
        let flags = unsafe { fcntl(target, F_GETFD) };
        let original = if flags == -1
            {
                let error = Error::last_os_error();
                if error.raw_os_error() != Some(EBADF) { return Err(error); }
                None
            }
            else
            {
                let saved = unsafe { fcntl(target, F_DUPFD_CLOEXEC, 3) };
                if saved == -1 { return Err(Error::last_os_error()); }
                // fcntl returned a new descriptor owned only by this guard.
                Some(unsafe { OwnedFd::from_raw_fd(saved) })
            };
        let saved = Self { target, original, flags, restore: true };
        duplicate(source, target)?;
        Ok(saved)
    }
}


impl Drop for SavedOutput
{
    fn drop(&mut self)
    {
        if !self.restore { return; }
        if let Some(original) = &self.original
        {
            let _ = duplicate(original.as_raw_fd(), self.target);
            unsafe { fcntl(self.target, F_SETFD, self.flags); }
        }
        else { unsafe { close(self.target); } }
    }
}


struct SavedPipeSignal(sigaction);


impl SavedPipeSignal
{
    fn reset() -> io::Result<Self>
    {
        let mut action: sigaction = unsafe { zeroed() };
        let mut previous: sigaction = unsafe { zeroed() };
        action.sa_sigaction = SIG_DFL;
        unsafe { sigemptyset(&mut action.sa_mask); }
        if unsafe { sigaction(SIGPIPE, &action, &mut previous) } == -1
        { return Err(Error::last_os_error()); }
        Ok(Self(previous))
    }
}


impl Drop for SavedPipeSignal
{
    fn drop(&mut self)
    { unsafe { sigaction(SIGPIPE, &self.0, null_mut()); } }
}


impl Interpreter
{
    pub(super) fn handle_exec(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let invalid = |message: &str| InterpreterError::new(location.clone(),
            ErrorWhat::InvalidOperand(message.into()));
        if    self.pipeline_depth != 0
           || self.captured_stdout.is_some()
           || self.exceptions.readline_callback
        {
            return Err(invalid("exec cannot replace the shell inside a pipeline or capture"));
        }
        if self.redirections.iter().any(|redirect|
            [redirect.output.as_ref(), redirect.error.as_ref()].into_iter().flatten()
                .any(|output| matches!(&**output, Output::Pipe(_))))
        {
            return Err(invalid("exec cannot redirect into a Shelly variable"));
        }
        let args = if args.first().is_some_and(|value| value.as_text() == "--")
            { &args[1..] } else { args };
        let arguments: Vec<String> = args.iter()
            .map(|argument| self.eval_path_from(&argument.as_text())).collect();
        if arguments.first().is_some_and(String::is_empty)
        {
            return Err(InterpreterError::new(location.clone(),
                ErrorWhat::ExecutableNotFound(String::new())));
        }
        let environment = if arguments.is_empty() { Vec::new() }
            else { self.exported_environment(location)? };
        if self.halted { return Ok(()); }
        let c_string = |text: &str| CString::new(text)
            .map_err(|_| invalid("exec arguments and environment cannot contain NUL bytes"));
        let argv = arguments.iter().map(|text| c_string(text)).collect::<Result<Vec<_>, _>>()?;
        let env = environment.iter().map(|(name, value)|
            {
                if name.is_empty() || name.contains('=')
                { return Err(invalid("exec environment contains an invalid variable name")); }
                c_string(&format!("{name}={value}"))
            }).collect::<Result<Vec<_>, _>>()?;
        let mut argv_ptrs: Vec<_> = argv.iter().map(|text| text.as_ptr()).collect();
        let mut env_ptrs: Vec<_> = env.iter().map(|text| text.as_ptr()).collect();
        argv_ptrs.push(null());
        env_ptrs.push(null());
        let paths = match arguments.first()
        {
            None => Vec::new(),
            Some(program) if program.contains('/') => vec![PathBuf::from(program)],
            Some(program) =>
                {
                    let path = environment.iter().find(|(name, _)| name == "PATH")
                        .map_or("/bin:/usr/bin", |(_, value)| value.as_str());
                    split_paths(path).map(|directory| directory.join(program)).collect()
                },
        };
        let paths = paths.iter().map(|path| CString::new(path.as_os_str().as_bytes())
            .map_err(|_| invalid("exec program path cannot contain NUL bytes")))
            .collect::<Result<Vec<_>, _>>()?;

        let io_error = |error| Self::redirection_error(location, error);
        stdout().flush().map_err(io_error)?;
        stderr().flush().map_err(io_error)?;
        let mut saved = Vec::new();
        for (target, output) in [
                (1, self.redirections.iter().rev().find_map(|redirect| redirect.output.as_ref())),
                (2, self.redirections.iter().rev().find_map(|redirect| redirect.error.as_ref())),
            ]
        {
            if let Some(output) = output
            {
                let Output::File(file) = &**output else { unreachable!(); };
                saved.push(SavedOutput::install(target, file.as_raw_fd()).map_err(io_error)?);
            }
        }
        if arguments.is_empty()
        {
            // Without a program, exec commits file redirections for subsequent commands.
            for output in &mut saved { output.restore = false; }
            self.last_result = Some(Value::ExecResult(ExecResult::Value(0)));
            return Ok(());
        }

        // Rust ignores SIGPIPE; executed programs should have ordinary Unix pipe behavior.
        // Caught handlers (including Shelly's SIGINT handler) reset on successful execve.
        let signal = SavedPipeSignal::reset().map_err(io_error)?;
        let mut error = Error::from_raw_os_error(ENOENT);
        let mut denied = false;
        for path in &paths
        {
            // All strings and terminated pointer arrays remain alive throughout the call.
            // Explicit envp avoids mutating the environment shared with native worker threads.
            unsafe { execve(path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()); }
            error = Error::last_os_error();
            match error.raw_os_error()
            {
                Some(EACCES) => denied = true,
                Some(ENOENT | ENOTDIR) => {},
                _ => break,
            }
        }
        if denied && matches!(error.raw_os_error(), Some(ENOENT | ENOTDIR))
        { error = Error::from_raw_os_error(EACCES); }
        else if !arguments[0].contains('/') && error.raw_os_error() == Some(ENOTDIR)
        { error = Error::from_raw_os_error(ENOENT); }
        drop(signal);
        drop(saved);
        Err(InterpreterError::new(location.clone(),
            if error.kind() == ErrorKind::NotFound
            { ErrorWhat::ExecutableNotFound(self.eval_path_to(&arguments[0])) }
            else { ErrorWhat::ExecutableIoError(format!("'{}': {}",
                self.eval_path_to(&arguments[0]), error)) }))
    }
}
