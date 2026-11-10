use mac_abi::{files, pipes, processes};

use super::env::{CommandEnv, CommandEnvs, CommandResolvedEnvs};
pub use crate::ffi::OsString as EnvKey;
use crate::{ffi::{OsStr, OsString},
            fmt, io,
            io::{BorrowedCursor, IoSlice, IoSliceMut, Read},
            num::NonZero,
            path::Path,
            process::StdioPipes,
            sys::{fs::File, pal::runtime, unsupported}};

////////////////////////////////////////////////////////////////////////////////
// Command
////////////////////////////////////////////////////////////////////////////////

pub struct Command
{
    program: OsString,
    args:    Vec<OsString>,
    env:     CommandEnv,

    cwd:    Option<OsString>,
    stdin:  Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>
}

#[derive(Debug)]
pub enum Stdio
{
    Inherit,
    Null,
    MakePipe,
    ParentStdout,
    ParentStderr,
    Pipe(ChildPipe),
    #[allow(dead_code)] // This variant exists only for the Debug impl
    InheritFile(File)
}

impl Command
{
    pub fn new(program: &OsStr) -> Command
    {
        Command { program: program.to_owned(),
                  args:    vec![program.to_owned()],
                  env:     Default::default(),
                  cwd:     None,
                  stdin:   None,
                  stdout:  None,
                  stderr:  None }
    }

    pub fn arg(&mut self, arg: &OsStr)
    {
        self.args.push(arg.to_owned());
    }

    pub fn env_mut(&mut self) -> &mut CommandEnv
    {
        &mut self.env
    }

    pub fn cwd(&mut self, dir: &OsStr)
    {
        self.cwd = Some(dir.to_owned());
    }

    pub fn stdin(&mut self, stdin: Stdio)
    {
        self.stdin = Some(stdin);
    }

    pub fn stdout(&mut self, stdout: Stdio)
    {
        self.stdout = Some(stdout);
    }

    pub fn stderr(&mut self, stderr: Stdio)
    {
        self.stderr = Some(stderr);
    }

    pub fn get_program(&self) -> &OsStr
    {
        &self.program
    }

    pub fn get_args(&self) -> CommandArgs<'_>
    {
        let mut iter = self.args.iter();
        iter.next();
        CommandArgs { iter }
    }

    pub fn get_envs(&self) -> CommandEnvs<'_>
    {
        self.env.iter()
    }

    pub fn get_env_clear(&self) -> bool
    {
        self.env.does_clear()
    }

    pub fn get_resolved_envs(&self) -> CommandResolvedEnvs
    {
        CommandResolvedEnvs::new(self.env.capture())
    }

    pub fn get_current_dir(&self) -> Option<&Path>
    {
        self.cwd.as_ref().map(|cs| Path::new(cs))
    }

    pub fn spawn(&mut self, default: Stdio, needs_stdin: bool)
                 -> io::Result<(Process, StdioPipes)>
    {
        if !self.env.is_unchanged() || self.cwd.is_some()
        {
            return unsupported();
        }
        let stdin = self.stdin.as_ref().unwrap_or(&default);
        let stdout = self.stdout.as_ref().unwrap_or(&default);
        let stderr = self.stderr.as_ref().unwrap_or(&default);
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        let path = self.program.to_str().ok_or_else(invalid)?;
        if self.args.len() > processes::ARGUMENT_COUNT
        {
            return Err(invalid());
        }
        let mut bytes = [0; processes::ARGUMENT_BYTES];
        let mut length = 0;
        for arg in &self.args
        {
            let arg = arg.to_str().ok_or_else(invalid)?.as_bytes();
            if arg.contains(&0) || arg.len() >= bytes.len() - length
            {
                return Err(invalid());
            }
            let end = length + arg.len();
            bytes[length..end].copy_from_slice(arg);
            length = end + 1;
        }
        let inherited = crate::env::var_os("PATH");
        let search_path = inherited.as_deref().and_then(OsStr::to_str).unwrap_or("");
        if search_path.len() > bytes.len() - length
        {
            return Err(invalid());
        }
        let entry = resolve_cli(path, search_path)?;
        let (mut child_in, parent_in) = ChildPipe::prepare(stdin, 0)?;
        let (mut child_out, parent_out) = ChildPipe::prepare(stdout, 1)?;
        let (mut child_err, parent_err) = ChildPipe::prepare(stderr, 2)?;
        let mut session = processes::ProcessCall { operation: processes::CONSOLE_SESSION,
                                                   ..Default::default() };
        runtime::client().process(&mut session).map_err(native_error)?;
        let mut call = processes::ProcessCall { operation: processes::LAUNCH,
                                                id: session.id,
                                                volume: entry.volume,
                                                cnid: entry.id,
                                                streams: [child_in.handle, child_out.handle,
                                                          child_err.handle],
                                                ..Default::default() };
        runtime::client().launch_argv(&mut call, &bytes[..length], self.args.len(), search_path)
                         .map_err(native_error)?;
        child_in.handle = 0;
        child_out.handle = 0;
        child_err.handle = 0;
        let stdin = if needs_stdin { parent_in } else { None };
        Ok((Process { child:  call.info.id,
                      result: None },
            StdioPipes { stdin,
                         stdout: parent_out,
                         stderr: parent_err }))
    }
}

fn resolve_cli(program: &str, search_path: &str) -> io::Result<files::CatalogInfo>
{
    let resolve = |path: &str| {
        let mut call = files::FileCall { operation: files::RESOLVE_PATH,
                                         ..Default::default() };
        runtime::client().file(&mut call, path, &mut []).map(|()| call.entry)
    };
    if program.contains(['/', ':'])
    {
        return resolve(program).map_err(native_error);
    }
    if program.is_empty()
    {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    for directory in search_path.split(':').filter(|directory| !directory.is_empty())
    {
        let candidate = Path::new(directory).join(program);
        let path =
            candidate.to_str().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        match resolve(path)
        {
            Ok(entry) if entry.file_type == *b"CLAP" => return Ok(entry),
            Ok(_) | Err(mac_traps::Error::NotFound) => (),
            Err(error) => return Err(native_error(error))
        }
    }
    Err(io::Error::from(io::ErrorKind::NotFound))
}

pub fn output(cmd: &mut Command) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)>
{
    let (mut process, mut streams) = cmd.spawn(Stdio::MakePipe, false)?;
    drop(streams.stdin.take());
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    match (streams.stdout.take(), streams.stderr.take())
    {
        (Some(out), Some(err)) => read_output(out, &mut stdout, err, &mut stderr)?,
        (Some(mut out), None) =>
        {
            out.read_to_end(&mut stdout)?;
        }
        (None, Some(mut err)) =>
        {
            err.read_to_end(&mut stderr)?;
        }
        (None, None) => ()
    }
    Ok((process.wait()?, stdout, stderr))
}

impl From<ChildPipe> for Stdio
{
    fn from(pipe: ChildPipe) -> Stdio
    {
        Stdio::Pipe(pipe)
    }
}

impl From<crate::sys::pipe::Pipe> for Stdio
{
    fn from(pipe: crate::sys::pipe::Pipe) -> Self
    {
        pipe.diverge()
    }
}

impl From<io::Stdout> for Stdio
{
    fn from(_: io::Stdout) -> Stdio
    {
        Stdio::ParentStdout
    }
}

impl From<io::Stderr> for Stdio
{
    fn from(_: io::Stderr) -> Stdio
    {
        Stdio::ParentStderr
    }
}

impl From<File> for Stdio
{
    fn from(file: File) -> Stdio
    {
        Stdio::InheritFile(file)
    }
}

impl fmt::Debug for Command
{
    // show all attributes
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        if f.alternate()
        {
            let mut debug_command = f.debug_struct("Command");
            debug_command.field("program", &self.program).field("args", &self.args);
            if !self.env.is_unchanged()
            {
                debug_command.field("env", &self.env);
            }

            if self.cwd.is_some()
            {
                debug_command.field("cwd", &self.cwd);
            }

            if self.stdin.is_some()
            {
                debug_command.field("stdin", &self.stdin);
            }
            if self.stdout.is_some()
            {
                debug_command.field("stdout", &self.stdout);
            }
            if self.stderr.is_some()
            {
                debug_command.field("stderr", &self.stderr);
            }

            debug_command.finish()
        }
        else
        {
            if let Some(ref cwd) = self.cwd
            {
                write!(f, "cd {cwd:?} && ")?;
            }
            if self.env.does_clear()
            {
                write!(f, "env -i ")?;
                // Altered env vars will be printed next, that should exactly work as expected.
            }
            else
            {
                // Removed env vars need the command to be wrapped in `env`.
                let mut any_removed = false;
                for (key, value_opt) in self.get_envs()
                {
                    if value_opt.is_none()
                    {
                        if !any_removed
                        {
                            write!(f, "env ")?;
                            any_removed = true;
                        }
                        write!(f, "-u {} ", key.to_string_lossy())?;
                    }
                }
            }
            // Altered env vars can just be added in front of the program.
            for (key, value_opt) in self.get_envs()
            {
                if let Some(value) = value_opt
                {
                    write!(f, "{}={value:?} ", key.to_string_lossy())?;
                }
            }
            if self.program != self.args[0]
            {
                write!(f, "[{:?}] ", self.program)?;
            }
            write!(f, "{:?}", self.args[0])?;

            for arg in &self.args[1..]
            {
                write!(f, " {:?}", arg)?;
            }
            Ok(())
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub struct ExitStatus
{
    code:        i32,
    aborted:     bool,
    interrupted: bool
}

impl ExitStatus
{
    pub fn exit_ok(&self) -> Result<(), ExitStatusError>
    {
        if self.code == 0 && !self.aborted && !self.interrupted
        {
            Ok(())
        }
        else
        {
            Err(ExitStatusError(*self))
        }
    }

    pub fn code(&self) -> Option<i32>
    {
        (!self.aborted && !self.interrupted).then_some(self.code)
    }
}

impl fmt::Display for ExitStatus
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        if self.interrupted
        {
            return f.write_str("interrupted by terminal");
        }
        if self.aborted
        {
            write!(f, "aborted with code {}", self.code)
        }
        else
        {
            write!(f, "exit code: {}", self.code)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitStatusError(ExitStatus);

impl From<ExitStatusError> for ExitStatus
{
    fn from(error: ExitStatusError) -> Self
    {
        error.0
    }
}

impl ExitStatusError
{
    pub fn code(self) -> Option<NonZero<i32>>
    {
        self.0.code().and_then(NonZero::new)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitCode(u8);

impl ExitCode
{
    pub const FAILURE: ExitCode = ExitCode(1);
    pub const SUCCESS: ExitCode = ExitCode(0);

    pub fn as_i32(&self) -> i32
    {
        self.0 as i32
    }
}

impl From<u8> for ExitCode
{
    fn from(code: u8) -> Self
    {
        Self(code)
    }
}

pub struct Process
{
    child:  u64,
    result: Option<ExitStatus>
}

impl Process
{
    pub fn id(&self) -> u32
    {
        self.child as u32
    }

    pub fn kill(&mut self) -> io::Result<()>
    {
        unsupported()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus>
    {
        if let Some(result) = self.result
        {
            return Ok(result);
        }
        let mut call = processes::ChildWaitCall { child: self.child,
                                                  ..Default::default() };
        runtime::client().wait_child(&mut call).map_err(native_error)?;
        self.save_result(call.state, call.code)?.ok_or(io::ErrorKind::InvalidData.into())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>
    {
        if self.result.is_some()
        {
            return Ok(self.result);
        }
        let mut call = processes::ProcessCall { operation: processes::CHILD_STATUS,
                                                id: self.child,
                                                ..Default::default() };
        runtime::client().process(&mut call).map_err(native_error)?;
        self.save_result(call.index, call.exit_code)
    }

    fn save_result(&mut self, state: u32, code: i32) -> io::Result<Option<ExitStatus>>
    {
        match state
        {
            processes::CHILD_RUNNING => Ok(None),
            processes::CHILD_EXITED | processes::CHILD_ABORTED | processes::CHILD_INTERRUPTED =>
            {
                self.result = Some(ExitStatus { code,
                                                aborted: state == processes::CHILD_ABORTED,
                                                interrupted: state
                                                             == processes::CHILD_INTERRUPTED });
                Ok(self.result)
            }
            _ => Err(io::ErrorKind::InvalidData.into())
        }
    }
}

impl Drop for Process
{
    fn drop(&mut self)
    {
        let _ = runtime::client().process(&mut processes::ProcessCall { operation:
                                                                            processes::CHILD_FORGET,
                                                                        id: self.child,
                                                                        ..Default::default() });
    }
}

pub struct CommandArgs<'a>
{
    iter: crate::slice::Iter<'a, OsString>
}

impl<'a> Iterator for CommandArgs<'a>
{
    type Item = &'a OsStr;

    fn next(&mut self) -> Option<&'a OsStr>
    {
        self.iter.next().map(|os| &**os)
    }

    fn size_hint(&self) -> (usize, Option<usize>)
    {
        self.iter.size_hint()
    }
}

impl<'a> ExactSizeIterator for CommandArgs<'a>
{
    fn len(&self) -> usize
    {
        self.iter.len()
    }

    fn is_empty(&self) -> bool
    {
        self.iter.is_empty()
    }
}

impl<'a> fmt::Debug for CommandArgs<'a>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.debug_list().entries(self.iter.clone()).finish()
    }
}

#[derive(Debug)]
pub struct ChildPipe
{
    handle: u64
}

impl ChildPipe
{
    fn duplicate(handle: u64) -> io::Result<Self>
    {
        if handle == 0
        {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let mut call = pipes::Call { operation: pipes::DUPLICATE,
                                     handle,
                                     ..Default::default() };
        runtime::client().pipes(&mut call).map_err(native_error)?;
        let handle = match (call.reader, call.writer)
        {
            (reader, 0) if reader != 0 => reader,
            (0, writer) if writer != 0 => writer,
            _ => return Err(io::ErrorKind::InvalidData.into())
        };
        Ok(Self { handle })
    }

    /// Every spawn receives fresh aliases. Failure drops only those aliases;
    /// neither the parent's standard handles nor reusable Command configuration
    /// are consumed. The resident launch validates direction and ownership.
    fn prepare(configuration: &Stdio, index: usize) -> io::Result<(Self, Option<Self>)>
    {
        match configuration
        {
            Stdio::Inherit => Ok((Self::duplicate(runtime::stream(index))?, None)),
            Stdio::ParentStdout => Ok((Self::duplicate(runtime::stream(1))?, None)),
            Stdio::ParentStderr => Ok((Self::duplicate(runtime::stream(2))?, None)),
            Stdio::Pipe(pipe) => Ok((Self::duplicate(pipe.handle)?, None)),
            Stdio::MakePipe =>
            {
                let (reader, writer) = Self::pair()?;
                if index == 0 { Ok((reader, Some(writer))) } else { Ok((writer, Some(reader))) }
            }
            Stdio::Null if index == 0 =>
            {
                let (reader, writer) = Self::pair()?;
                drop(writer);
                Ok((reader, None))
            }
            // A readerless pipe is BrokenPipe, not a null output sink.
            Stdio::Null | Stdio::InheritFile(_) => unsupported()
        }
    }

    fn pair() -> io::Result<(Self, Self)>
    {
        let mut call = pipes::Call { operation: pipes::CREATE,
                                     capacity: 4096,
                                     ..Default::default() };
        runtime::client().pipes(&mut call).map_err(native_error)?;
        Ok((Self { handle: call.reader }, Self { handle: call.writer }))
    }

    fn transfer(&self, operation: u32, buffer: *mut u8, length: usize) -> io::Result<usize>
    {
        if length == 0
        {
            return Ok(0);
        }
        let mut call = pipes::Call { operation,
                                     handle: self.handle,
                                     buffer,
                                     length,
                                     ..Default::default() };
        runtime::client().pipes(&mut call).map_err(native_error)?;
        Ok(call.transferred)
    }

    pub fn read(&self, output: &mut [u8]) -> io::Result<usize>
    {
        self.transfer(pipes::READ, output.as_mut_ptr(), output.len())
    }

    pub fn read_buf(&self, mut output: BorrowedCursor<'_, u8>) -> io::Result<()>
    {
        let count = self.read(output.ensure_init())?;
        output.advance_checked(count);
        Ok(())
    }

    pub fn read_vectored(&self, outputs: &mut [IoSliceMut<'_>]) -> io::Result<usize>
    {
        match outputs.iter_mut().find(|buffer| !buffer.is_empty())
        {
            Some(output) => self.read(output),
            None => Ok(0)
        }
    }

    pub fn write(&self, input: &[u8]) -> io::Result<usize>
    {
        self.transfer(pipes::WRITE, input.as_ptr().cast_mut(), input.len())
    }

    pub fn write_vectored(&self, inputs: &[IoSlice<'_>]) -> io::Result<usize>
    {
        match inputs.iter().find(|buffer| !buffer.is_empty())
        {
            Some(input) => self.write(input),
            None => Ok(0)
        }
    }

    pub fn is_read_vectored(&self) -> bool
    {
        false
    }

    pub fn is_write_vectored(&self) -> bool
    {
        false
    }
}

impl Drop for ChildPipe
{
    fn drop(&mut self)
    {
        if self.handle != 0
        {
            let _ = runtime::client().pipes(&mut pipes::Call { operation: pipes::CLOSE,
                                                               handle: self.handle,
                                                               ..Default::default() });
        }
    }
}

impl Read for ChildPipe
{
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize>
    {
        ChildPipe::read(self, output)
    }
}

pub fn read_output(mut out: ChildPipe, stdout: &mut Vec<u8>, mut err: ChildPipe,
                   stderr: &mut Vec<u8>)
                   -> io::Result<()>
{
    crate::thread::scope(|scope| {
        let reader =
            crate::thread::Builder::new().spawn_scoped(scope, move || out.read_to_end(stdout))?;
        let stderr = err.read_to_end(stderr);
        reader.join().map_err(|_| io::Error::other("stdout reader aborted"))??;
        stderr?;
        Ok(())
    })
}

fn native_error(error: mac_traps::Error) -> io::Error
{
    io::Error::from_raw_os_error(error.status())
}

pub fn getpid() -> u32
{
    let mut call = processes::ProcessCall { operation: processes::CURRENT,
                                            ..Default::default() };
    if runtime::client().process(&mut call).is_ok() { call.info.id as u32 } else { 0 }
}
