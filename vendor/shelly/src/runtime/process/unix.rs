
use std::{ cell::RefCell,
           cmp::Ordering,
           collections::HashMap,
           fs::File,
           hash::{ Hash, Hasher },
           io::{ self, Read, Write, ErrorKind },
           mem::zeroed,
           ptr::null_mut,
           str::from_utf8,
           os::{ fd::{ AsRawFd, FromRawFd }, unix::process::{ CommandExt, ExitStatusExt } },
           process::{ Child, Command, ExitStatus, Stdio },
           rc::Rc,
           sync::atomic::{ AtomicU64, Ordering as AtomicOrdering },
           thread::sleep,
           time::{ Duration, Instant } };

use libc::{ fcntl, ioctl, kill, openpty, poll as poll_descriptors, pollfd, setsid, siginfo_t,
            waitid, winsize, EIO, ESRCH, FD_CLOEXEC, F_DUPFD_CLOEXEC, F_GETFL, F_SETFD,
            F_SETFL, O_NONBLOCK, POLLIN, POLLOUT, P_PID, SIGINT, SIGKILL, SIGTERM, TIOCSCTTY,
            WEXITED, WNOHANG, WNOWAIT };

#[cfg(target_os = "macos")]
use libc::{ proc_bsdshortinfo, proc_listpgrppids, proc_pidinfo, EPERM,
            PROC_PIDT_SHORTBSDINFO, SZOMB };

use crate::{ language::data::{ value::Value, map_key::MapKey },
             runtime::signals::take_interrupt };

fn record(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value
{
    Value::from_hash_map(entries.into_iter()
        .map(|(key, value)| (MapKey::String(key.to_string()), value)).collect())
}

fn string(value: &Value) -> Result<&str, String>
{
    match value
    {
        Value::String(value, _) => Ok(value),
        _ => Err("Expected a String".to_string()),
    }
}

fn milliseconds(value: &Value) -> Result<Duration, String>
{
    match value
    {
        Value::Integer(value) if *value >= 0 => Ok(Duration::from_millis(*value as u64)),
        _ => Err("Timeout must be a nonnegative Integer in milliseconds".to_string()),
    }
}

struct Options<'a>(&'a HashMap<MapKey, Value>);
impl<'a> Options<'a>
{
    fn get(&self, key: &str) -> Option<&'a Value>
    {
        self.0.get(&MapKey::String(key.to_string()))
    }
    fn duration(&self, key: &str) -> Result<Option<Duration>, String>
    {
        self.get(key).map(milliseconds).transpose()
    }
    fn dimension(&self, key: &str, default: u16) -> Result<u16, String>
    {
        match self.get(key)
        {
            None => Ok(default),
            Some(Value::Integer(value)) if (1..=u16::MAX as i64).contains(value) =>
                Ok(*value as u16),
            _ => Err(format!("{} must be an Integer from 1 to 65535", key)),
        }
    }
}

fn prepare<'a>(args: &'a [Value], terminal: bool, environment: &[(String, String)],
               expand: &impl Fn(&str) -> String)
           -> Result<(Command, Options<'a>), String>
{
    let [Value::Array(argv), Value::HashMap(options)] = args else
    { return Err("Expected an argument array and an options map".to_string()); };
    let Some(first) = argv.first() else
    { return Err("Command array cannot be empty".to_string()); };
    let argv: Vec<String> = argv.iter()
        .map(|arg| string(arg).map(expand)).collect::<Result<_, _>>()?;
    if string(first)?.is_empty() { return Err("Executable cannot be empty".to_string()); }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).env_clear().envs(environment.iter().map(|(key, value)| (key, value)));
    let options = Options(options);
    for (key, value) in options.0
    {
        let MapKey::String(key) = key else
        { return Err("Option keys must be strings".to_string()); };
        match key.as_str()
        {
            "cwd" => { command.current_dir(expand(string(value)?)); },
            "env" =>
            {
                let Value::HashMap(environment) = value else
                { return Err("env must be a map of String names and values".to_string()); };
                command.env_clear();
                for (name, value) in environment.iter()
                {
                    let MapKey::String(name) = name else
                    { return Err("Environment names must be strings".to_string()); };
                    if    name.is_empty()
                       || name.contains(['=', '\0'])
                       || string(value)?.contains('\0')
                    { return Err("Invalid environment name or value".to_string()); }
                    command.env(name, expand(string(value)?));
                }
            },
            "stdin_file" | "stdout_file" | "stderr_file" if !terminal =>
                { string(value)?; },
            "timeout_ms" if !terminal => { milliseconds(value)?; },
            "rows" | "columns" if terminal => { options.dimension(key, 1)?; },
            _ => return Err(format!("Unknown {} option: {}",
                if terminal { "terminal" } else { "process" }, key)),
        }
    }
    Ok((command, options))
}

fn outcome(status: Option<ExitStatus>, timed_out: bool, error: Option<String>) -> Value
{
    record([
            ("exit_code", status.and_then(|value| value.code())
                .map_or(Value::None, |value| Value::Integer(value as i64))),
            ("signal", status.and_then(|value| value.signal())
                .map_or(Value::None, |value| Value::Integer(value as i64))),
            ("timed_out", Value::Boolean(timed_out)),
            ("error", error.map_or(Value::None, Value::from_string)),
        ])
}

// The child owns a separate process group. Always stop descendants too, even if
// the group leader has exited. Cleanup happens before releasing the child PID.
#[derive(Debug)]
struct ManagedChild
{
    child: Child,
    stopped: bool,
}
impl ManagedChild
{
    fn exited(&self) -> io::Result<bool>
    {
        if self.stopped { return Ok(true); }
        // Observe exit without releasing the PID: stop the group before reaping
        // its leader, so PID reuse cannot target an unrelated process group.
        // SAFETY: an all-zero siginfo_t is valid input/output storage for waitid.
        let mut information: siginfo_t = unsafe { zeroed() };
        if unsafe { waitid(P_PID, self.child.id(), &mut information,
            WEXITED | WNOHANG | WNOWAIT) } == -1
        { return Err(io::Error::last_os_error()); }
        Ok(unsafe { information.si_pid() } != 0)
    }

    fn signal(&self, signal: i32) -> io::Result<()>
    {
        // SAFETY: the negative PID addresses the process group we created.
        if unsafe { kill(-(self.child.id() as i32), signal) } == -1
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ESRCH) { return Ok(()); }
            // Darwin can return EPERM while the group is exiting after a PTY
            // hangup, before waitid reports the leader's exit. Allow that small
            // race to settle, but require every member to be gone or a zombie:
            // a denied group signal must never fall back to killing only its leader.
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(EPERM)
            {
                let deadline = Instant::now() + Duration::from_millis(100);
                loop
                {
                    if self.group_exited() { return Ok(()); }
                    if Instant::now() >= deadline { break; }
                    sleep(Duration::from_millis(2));
                }
            }
            return Err(io::Error::new(error.kind(), format!(
                "Cannot send signal {} to process group {}: {}", signal, self.child.id(), error)));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn group_exited(&self) -> bool
    {
        // Keep the exited leader unreaped while inspecting its group: its PID
        // cannot be recycled. If inspection fails, preserve the original EPERM.
        if !self.exited().unwrap_or(false) { return false; }
        let group = self.child.id() as i32;
        // SAFETY: a null buffer requests the required PID count, including slack.
        let capacity = unsafe { proc_listpgrppids(group, null_mut(), 0) };
        if capacity <= 0 { return false; }
        let mut members = vec![0_i32; capacity as usize];
        let Ok(bytes) = i32::try_from(members.len() * size_of::<i32>())
            else { return false; };
        // SAFETY: members owns writable, correctly aligned storage of bytes length.
        let count = unsafe { proc_listpgrppids(group, members.as_mut_ptr().cast(), bytes) };
        // Zero also denotes an API error. A full buffer may have been truncated.
        if count <= 0 || count as usize >= members.len() { return false; }
        for &pid in &members[..count as usize]
        {
            // SAFETY: this C record consists of integers and byte arrays.
            let mut information: proc_bsdshortinfo = unsafe { zeroed() };
            let bytes = size_of::<proc_bsdshortinfo>() as i32;
            // SAFETY: the output buffer has the declared size. Argument 1 includes
            // zombies, which a normal proc_pidinfo lookup would omit.
            let copied = unsafe { proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 1,
                (&raw mut information).cast(), bytes) };
            if copied != bytes
            {
                if copied == 0 && io::Error::last_os_error().raw_os_error() == Some(ESRCH)
                { continue; }
                return false;
            }
            if information.pbsi_pgid == group as u32 && information.pbsi_status != SZOMB
            { return false; }
        }
        true
    }

    fn stop(&mut self) -> io::Result<ExitStatus>
    {
        if !self.stopped
        {
            self.signal(SIGKILL)?;
            self.stopped = true;
        }
        self.child.wait()
    }
}
impl Drop for ManagedChild
{
    fn drop(&mut self)
    {
        let _ = self.stop();
    }
}

fn run(args: &[Value], environment: &[(String, String)], expand: &impl Fn(&str) -> String,
       configure: &mut impl FnMut(&mut Command) -> io::Result<()>) -> Result<Value, String>
{
    let (mut command, options) = prepare(args, false, environment, expand)?;
    let timeout = options.duration("timeout_ms")?;
    // Open all input files before truncating outputs. Paths are relative to the caller.
    let setup = (|| -> io::Result<()>
        {
            configure(&mut command)?;
            if let Some(value) = options.get("stdin_file")
            { command.stdin(File::open(expand(string(value).unwrap()))?); }
            for (name, output) in [("stdout_file", true), ("stderr_file", false)]
            {
                if let Some(value) = options.get(name)
                {
                    let path = expand(string(value).unwrap());
                    let file = match path.as_str()
                    {
                        "/dev/stdout" | "/dev/stderr" =>
                        {
                            let fd = if path == "/dev/stdout" { 1 } else { 2 };
                            // Duplicate the stream without truncating its underlying file.
                            let fd = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, 3) };
                            if fd == -1 { return Err(io::Error::last_os_error()); }
                            unsafe { File::from_raw_fd(fd) }
                        },
                        _ => File::create(path)?,
                    };
                    if output { command.stdout(file); } else { command.stderr(file); }
                }
            }
            Ok(())
        })();
    if let Err(error) = setup { return Ok(outcome(None, false, Some(error.to_string()))); }
    command.process_group(0);
    let child = match command.spawn()
    {
        Ok(child) => child,
        Err(error) => return Ok(outcome(None, false, Some(error.to_string()))),
    };
    let mut child = ManagedChild { child, stopped: false };
    let started = Instant::now();
    loop
    {
        // Supervised processes have their own group and do not receive terminal SIGINT.
        if take_interrupt() { child.signal(SIGINT).map_err(|error| error.to_string())?; }
        if child.exited().map_err(|error| error.to_string())?
        {
            let status = child.stop().map_err(|error| error.to_string())?;
            return Ok(outcome(Some(status), false, None));
        }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout)
        {
            child.signal(SIGTERM).map_err(|error| error.to_string())?;
            sleep(Duration::from_millis(100));
            let status = child.stop().map_err(|error| error.to_string())?;
            return Ok(outcome(Some(status), true, None));
        }
        sleep(Duration::from_millis(2));
    }
}

#[derive(Debug)]
struct TerminalState
{
    master: Option<File>,
    child: ManagedChild,
    pending: Vec<u8>,
    eof: bool,
    closed: bool,
}

#[derive(Debug)]
pub struct Terminal
{
    // Only this immutable identity participates in equality, ordering, and hashing.
    id: u64,
    state: RefCell<TerminalState>,
}
impl PartialEq for Terminal
{
    fn eq(&self, other: &Self) -> bool { self.id == other.id }
}
impl Eq for Terminal {}
impl PartialOrd for Terminal
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for Terminal
{
    fn cmp(&self, other: &Self) -> Ordering { self.id.cmp(&other.id) }
}
impl Hash for Terminal
{
    fn hash<H: Hasher>(&self, state: &mut H) { self.id.hash(state); }
}

fn open(args: &[Value], environment: &[(String, String)],
        expand: &impl Fn(&str) -> String) -> Result<Value, String>
{
    let (mut command, options) = prepare(args, true, environment, expand)?;
    let mut size = winsize
        {
            ws_row: options.dimension("rows", 40)?,
            ws_col: options.dimension("columns", 140)?,
            ws_xpixel: 0, ws_ypixel: 0,
        };
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: pointers refer to live output integers and a valid winsize; null
    // optional name/termios arguments request the system defaults.
    // macOS requires mutable pointers; Linux accepts these as const pointers.
    if unsafe { openpty(&mut master, &mut slave, null_mut(),
        null_mut(), &raw mut size) } == -1
    { return Err(io::Error::last_os_error().to_string()); }
    // SAFETY: openpty returned two new owned file descriptors.
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()]
    {
        if unsafe { fcntl(fd, F_SETFD, FD_CLOEXEC) } == -1
        { return Err(io::Error::last_os_error().to_string()); }
    }
    command.stdin(slave.try_clone().map_err(|error| error.to_string())?);
    command.stdout(slave.try_clone().map_err(|error| error.to_string())?);
    command.stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe syscalls are used between fork and exec.
    unsafe
    {
        command.pre_exec(||
            {
                // The ioctl request type differs between Unix platforms.
                if setsid() == -1 || ioctl(0, TIOCSCTTY as _, 0) == -1
                { return Err(io::Error::last_os_error()); }
                Ok(())
            });
    }
    let child = command.spawn().map_err(|error| error.to_string())?;
    let child = ManagedChild { child, stopped: false };
    let flags = unsafe { fcntl(master.as_raw_fd(), F_GETFL) };
    if flags == -1 || unsafe { fcntl(master.as_raw_fd(), F_SETFL,
        flags | O_NONBLOCK) } == -1
    { return Err(io::Error::last_os_error().to_string()); }
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    Ok(Value::Terminal(Rc::new(Terminal
        {
            id: NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed),
            state: RefCell::new(TerminalState
                {
                    master: Some(master),
                    child,
                    pending: Vec::new(),
                    eof: false,
                    closed: false,
                }),
        })))
}

fn poll(file: &File, events: i16, timeout: Duration) -> io::Result<bool>
{
    let mut descriptor = pollfd { fd: file.as_raw_fd(), events, revents: 0 };
    let timeout = timeout.as_millis().min(i32::MAX as u128) as i32;
    let result = unsafe { poll_descriptors(&mut descriptor, 1, timeout) };
    if result < 0 { return Err(io::Error::last_os_error()); }
    Ok(result > 0)
}

pub fn invoke(name: &str, args: &[Value], environment: &[(String, String)],
              expand: impl Fn(&str) -> String,
              mut configure: impl FnMut(&mut Command) -> io::Result<()>) -> Result<Value, String>
{
    match name
    {
        "run_process" => return run(args, environment, &expand, &mut configure),
        "open_terminal" => return open(args, environment, &expand),
        _ => (),
    }
    let (terminal, rest) = match args.split_first()
    {
        Some((Value::Terminal(terminal), rest)) => (terminal, rest),
        _ => return Err("Expected a Terminal handle".to_string()),
    };
    let mut state = terminal.state.borrow_mut();
    if name == "terminal_close" && rest.is_empty()
    {
        state.closed = true;
        state.master.take();
        let status = state.child.stop().map_err(|error| error.to_string())?;
        return Ok(outcome(Some(status), false, None));
    }
    if state.closed { return Err("Terminal is closed".to_string()); }
    match (name, rest)
    {
        ("terminal_write", [value]) =>
        {
            let mut bytes = string(value)?.as_bytes();
            let started = Instant::now();
            while !bytes.is_empty()
            {
                match state.master.as_mut().unwrap().write(bytes)
                {
                    Ok(0) => return Err("Terminal write made no progress".to_string()),
                    Ok(count) => bytes = &bytes[count..],
                    Err(error) if error.kind() == ErrorKind::WouldBlock =>
                    {
                        if started.elapsed() >= Duration::from_secs(5)
                        { return Err("Terminal write timed out".to_string()); }
                        poll(state.master.as_ref().unwrap(), POLLOUT, Duration::from_millis(10))
                            .map_err(|error| error.to_string())?;
                    },
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(Value::None)
        },
        ("terminal_read", [timeout]) =>
        {
            let timeout = milliseconds(timeout)?;
            let ready = state.eof
                || poll(state.master.as_ref().unwrap(), POLLIN, timeout)
                    .map_err(|error| error.to_string())?;
            let mut bytes = [0; 8192];
            if ready && !state.eof
            {
                match state.master.as_mut().unwrap().read(&mut bytes)
                {
                    Ok(0) => state.eof = true,
                    Ok(count) => state.pending.extend_from_slice(&bytes[..count]),
                    Err(error) if error.raw_os_error() == Some(EIO) => state.eof = true,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => (),
                    Err(error) => return Err(error.to_string()),
                }
            }
            let valid = match from_utf8(&state.pending)
            {
                Ok(_) => state.pending.len(),
                Err(error) if error.error_len().is_none() && !state.eof => error.valid_up_to(),
                Err(error) => return Err(format!("Terminal output is not UTF-8: {}", error)),
            };
            let text = String::from_utf8(state.pending.drain(..valid).collect()).unwrap();
            let status = if state.child.exited().map_err(|error| error.to_string())?
                { Some(state.child.stop().map_err(|error| error.to_string())?) }
                else { None };
            let Value::HashMap(result) = outcome(status, false, None) else { unreachable!(); };
            let mut result = (*result).clone();
            for (key, value) in [("text", Value::from_string(text)),
                ("eof", Value::Boolean(state.eof)), ("timed_out", Value::Boolean(!ready))]
            { result.insert(MapKey::String(key.to_string()), value); }
            Ok(Value::from_hash_map(result.values))
        },
        _ => Err(format!("Invalid arguments for {}", name)),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests
{
    use super::*;

    #[test]
    fn completed_process_group_can_be_reaped()
    {
        let child = Command::new("/usr/bin/true").process_group(0).spawn().unwrap();
        let mut child = ManagedChild { child, stopped: false };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.exited().unwrap()
        {
            assert!(Instant::now() < deadline, "child did not exit");
            sleep(Duration::from_millis(2));
        }
        assert!(child.group_exited(), "completed group is still reported alive");
        assert!(child.stop().unwrap().success());
    }

    #[test]
    fn closing_terminal_reaps_its_child()
    {
        for _ in 0..32
        {
            let terminal = open(&[
                Value::from_array(vec![Value::from_string("/bin/sleep".into()),
                    Value::from_string("30".into())]),
                Value::from_hash_map(HashMap::new()),
            ], &[], &str::to_string).unwrap();
            let result = invoke("terminal_close", &[terminal], &[], str::to_string,
                |_| Ok(()));
            assert!(result.is_ok(), "terminal cleanup failed: {result:?}");
        }
    }

    #[test]
    fn exited_leader_with_live_descendant_requires_group_cleanup()
    {
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
            .stdout(Stdio::piped()).process_group(0).spawn().unwrap();
        let mut child = ManagedChild { child, stopped: false };
        let mut pid = String::new();
        child.child.stdout.take().unwrap().read_to_string(&mut pid).unwrap();
        let pid: i32 = pid.trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.exited().unwrap()
        {
            assert!(Instant::now() < deadline, "leader did not exit");
            sleep(Duration::from_millis(2));
        }
        assert!(!child.group_exited(), "live descendant was treated as an exited group");
        assert!(child.stop().unwrap().success());
        loop
        {
            // SAFETY: information owns aligned storage of the declared size.
            let mut information: proc_bsdshortinfo = unsafe { zeroed() };
            let bytes = size_of::<proc_bsdshortinfo>() as i32;
            let copied = unsafe { proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 1,
                (&raw mut information).cast(), bytes) };
            if copied == bytes && information.pbsi_status == SZOMB { break; }
            if copied == 0 && io::Error::last_os_error().raw_os_error() == Some(ESRCH)
            { break; }
            assert_eq!(copied, bytes, "cannot inspect descendant");
            assert!(Instant::now() < deadline, "live descendant survived cleanup");
            sleep(Duration::from_millis(2));
        }
    }
}
