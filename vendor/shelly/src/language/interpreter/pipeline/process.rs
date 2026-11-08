
use std::{ io::{ Read, Write, Error, ErrorKind },
           os::{ fd::{ AsRawFd, RawFd }, unix::process::{ CommandExt, ExitStatusExt } },
           process::{ Child, ChildStdin, ChildStdout, Command, Stdio } };

use libc::{ fcntl, kill, pid_t, F_GETFL, F_SETFL, O_NONBLOCK, SIGINT, SIGKILL };

use crate::runtime::process::Foreground;
use crate::language::{ bytecode::PipelineStage,
                       data::value::{ ExecResult, Value },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       interpreter::{ pipeline::{ Context, Data, Step },
                                      redirection::{ configure, Redirection } },
                       text::location::Location };


pub(super) struct External
{
    child: Child,
    group: bool,
    foreground: Option<Foreground>,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    bytes: Vec<u8>,
    offset: usize,
    input_closed: bool,
    status: Option<ExecResult>,
    recorded: bool,
    location: Location,
    redirects: Vec<Redirection>,
}


fn nonblocking(fd: RawFd) -> Result<(), Error>
{
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags == -1 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1
    { return Err(Error::last_os_error()); }
    Ok(())
}


impl External
{
    pub fn new(interpreter: &mut Interpreter, stage: &PipelineStage, name: &str,
               arguments: &[Value], source: bool) -> InterpreterResult<Option<Self>>
    {
        let environment = interpreter.exported_environment(&stage.location)?;
        if interpreter.halted { return Ok(None); }
        let executable = interpreter.eval_path_from(name);
        let mut command = Command::new(&executable);
        if interpreter.pipeline_worker { command.process_group(0); }
        command.args(arguments.iter()
                .map(|argument| interpreter.eval_path_from(&argument.as_text())))
            .env_clear().envs(environment)
            .stdin(if source { Stdio::inherit() } else { Stdio::piped() })
            .stdout(Stdio::piped()).stderr(Stdio::inherit());
        let depth = interpreter.redirections.len();
        let redirects = (||
            {
                configure(&mut command, &interpreter.redirections)
                    .map_err(|error| Interpreter::redirection_error(&stage.location, error))?;
                command.stdout(Stdio::piped());
                for redirect in &stage.redirects
                {
                    interpreter.execute_instructions(&redirect.code)?;
                    let target = interpreter.last_result.take().unwrap_or(Value::None);
                    interpreter.begin_redirection(&redirect.location, redirect.stream,
                        redirect.variable, target)?;
                }
                // An enclosing redirection belongs to the entire pipeline; intermediate
                // stdout must still reach the next stage.
                configure(&mut command, &interpreter.redirections[depth..])
                    .map_err(|error| Interpreter::redirection_error(&stage.location, error))?;
                Ok(())
            })();
        if let Err(error) = redirects
        { let _ = interpreter.finish_redirections(depth); return Err(error); }
        let child = command.spawn().map_err(|error| InterpreterError::new(
            stage.location.clone(),
            if error.kind() == ErrorKind::NotFound
            { ErrorWhat::ExecutableNotFound(interpreter.eval_path_to(&executable)) }
            else { ErrorWhat::ExecutableIoError(error.to_string()) }
        ));
        // Process-specific captures remain alive until that process has completed.
        // The stream owns their shell endpoints; no endpoint leaks into later stages.
        let redirects = interpreter.redirections.split_off(depth);
        let mut child = match child
            {
                Ok(child) => child,
                Err(error) =>
                    {
                        interpreter.redirections.extend(redirects);
                        let _ = interpreter.finish_redirections(depth);
                        return Err(error);
                    },
            };
        let mut stream = Self
            {
                group: interpreter.pipeline_worker, foreground: None,
                input: child.stdin.take(), output: child.stdout.take(), child,
                bytes: Vec::new(), offset: 0, input_closed: source, status: None, recorded: false,
                location: stage.location.clone(), redirects,
            };
        if source && stream.group
        {
            stream.foreground = Foreground::acquire(stream.child.id() as pid_t)
                .map_err(|error| Interpreter::redirection_error(&stage.location, error))?;
        }
        if let Some(input) = &stream.input { nonblocking(input.as_raw_fd())
            .map_err(|error| Interpreter::redirection_error(&stage.location, error))?; }
        if let Some(output) = &stream.output { nonblocking(output.as_raw_fd())
            .map_err(|error| Interpreter::redirection_error(&stage.location, error))?; }
        Ok(Some(stream))
    }

    fn stop_group(&mut self)
    {
        if self.group
        {
            self.group = false;
            unsafe { kill(-(self.child.id() as pid_t), SIGKILL); }
        }
    }

    pub fn wants_input(&self) -> bool
    { !self.input_closed && self.input.is_some() && self.bytes.is_empty() }

    pub fn feed(&mut self, bytes: Vec<u8>) { self.bytes = bytes; self.offset = 0; }
    pub fn close_input(&mut self)
    {
        self.input_closed = true;
        if self.bytes.is_empty() { self.input.take(); }
    }

    pub fn poll(&mut self, interpreter: &mut Interpreter, context: &mut Context, index: usize)
        -> InterpreterResult<Step>
    {
        let error = |error: Error| Interpreter::redirection_error(&self.location, error);
        if    self.status.is_none()
           && let Some(status) = self.child.try_wait().map_err(error)?
        {
            self.status = Some(match status.code()
                {
                    Some(code) => ExecResult::Value(code as u8),
                    None => ExecResult::Signaled,
                });
            if self.foreground.is_some() && status.signal() == Some(SIGINT)
            {
                return Err(InterpreterError::new(self.location.clone(),
                    ErrorWhat::Interrupted));
            }
        }
        if let Some(input) = self.input.as_mut() && !self.bytes.is_empty()
        {
            match input.write(&self.bytes[self.offset..])
            {
                Ok(count) => self.offset += count,
                Err(error) if matches!(error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::Interrupted) => {},
                Err(error) if error.kind() == ErrorKind::BrokenPipe =>
                    {
                        self.input.take(); self.input_closed = true;
                        self.bytes.clear(); self.offset = 0;
                    },
                Err(value) => return Err(error(value)),
            }
            if self.offset == self.bytes.len() { self.bytes.clear(); self.offset = 0; }
        }
        if self.input_closed && self.bytes.is_empty() { self.input.take(); }
        if let Some(output) = self.output.as_mut()
        {
            let mut bytes = vec![0; 8192];
            match output.read(&mut bytes)
            {
                Ok(0) => { self.output.take(); },
                Ok(count) => { bytes.truncate(count); return Ok(Step::Data(Data::Bytes(bytes))); },
                Err(error) if matches!(error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::Interrupted) => {},
                Err(value) => return Err(error(value)),
            }
        }
        if self.output.is_none() && let Some(status) = &self.status
        {
            if !self.recorded { context.record(index, status.clone()); self.recorded = true; }
            self.finish_redirects(interpreter)?;
            return Ok(Step::End);
        }
        Ok(Step::Pending)
    }

    fn finish_redirects(&mut self, interpreter: &mut Interpreter) -> InterpreterResult<()>
    {
        let mut result = Ok(());
        while let Some(redirect) = self.redirects.pop()
        {
            let location = redirect.location.clone();
            let finished = redirect.finish()
                .map_err(|error| Interpreter::redirection_error(&location, error))
                .and_then(|capture| match capture
                    {
                        Some((name, text)) => interpreter.write_variable(&location,
                            &name, Value::from_string(text)),
                        None => Ok(()),
                    });
            result = result.and(finished);
        }
        result
    }

    pub fn cancel(&mut self, interpreter: &mut Interpreter, context: &mut Context, index: usize)
        -> InterpreterResult<()>
    {
        self.input.take(); self.output.take();
        self.stop_group();
        if self.status.is_none()
        { self.status = self.child.try_wait().ok().flatten().map(|status|
            match status.code() { Some(code) => ExecResult::Value(code as u8),
                                  None => ExecResult::Signaled }); }
        let canceled_after_failure = self.status.is_none() && context.failed();
        if self.status.is_none()
        {
            let _ = self.child.kill();
            self.status = self.child.wait().ok().map(|status|
                match status.code() { Some(code) => ExecResult::Value(code as u8),
                                      None => ExecResult::Signaled });
        }
        if !self.recorded && !canceled_after_failure
        {
            context.record(index, self.status.clone().unwrap_or(ExecResult::Signaled));
            self.recorded = true;
        }
        self.foreground.take();
        self.finish_redirects(interpreter)
    }
}


impl Drop for External
{
    fn drop(&mut self)
    {
        self.input.take(); self.output.take();
        self.stop_group();
        if self.status.is_none() { let _ = self.child.kill(); let _ = self.child.wait(); }
        self.foreground.take();
        for redirect in self.redirects.drain(..) { let _ = redirect.finish(); }
    }
}
