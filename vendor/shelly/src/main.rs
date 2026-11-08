#[cfg(not(target_os = "systemrisc"))]
use std::io::{IsTerminal, stderr};
use std::{env::args_os,
          fs::File,
          io::{BufReader, stdin},
          path::PathBuf,
          process::ExitCode};

use clap::Parser;
use supports_color::{Stream, on};

use crate::{language::{interpreter::{Interactive, Interpreter, RcFile, Startup},
                       text::{buffer::{Buffer, SimpleBuffer},
                              read_buffer::ReadBuffer}},
            runtime::{color::TtyColorMode,
                      repl::Repl,
                      result::{RuntimeError, RuntimeResult},
                      signals::InteractiveInterrupts}};

mod language;
mod runtime;
mod server;

// Opt-in interactive startup diagnostics. Record without phase-by-phase I/O,
// then flush after capturing the first editor-read timestamp.
mod startup_trace
{
    use std::{cell::RefCell, fmt::Write as _, time::Instant};
    #[derive(Clone, Copy)]
    struct Record
    {
        phase:  &'static str,
        micros: u128
    }
    struct Trace
    {
        start:   Option<Instant>,
        records: [Record; 32],
        length:  usize,
        dropped: usize
    }
    impl Trace
    {
        const fn new() -> Self
        {
            Self { start:   None,
                   records: [Record { phase:  "",
                                     micros: 0 }; 32],
                   length:  0,
                   dropped: 0 }
        }
    }
    thread_local! {
        static TRACE: RefCell<Trace> = const { RefCell::new(Trace::new()) };
    }
    pub(crate) fn begin()
    {
        if option_env!("SHELLY_PROFILE_STARTUP").is_some()
        {
            let start = Instant::now();
            TRACE.with(|state| state.borrow_mut().start = Some(start));
            mark("main");
        }
    }
    pub(crate) fn mark(phase: &'static str)
    {
        if option_env!("SHELLY_PROFILE_STARTUP").is_none()
        {
            return;
        }
        let start = TRACE.with(|state| state.borrow().start);
        let Some(start) = start
        else
        {
            return;
        };
        // No RefCell borrow crosses the actual platform clock call.
        let micros = start.elapsed().as_micros();
        TRACE.with(|state| {
                 let mut trace = state.borrow_mut();
                 let index = trace.length;
                 if index < trace.records.len()
                 {
                     trace.records[index] = Record { phase,
                                                     micros };
                     trace.length += 1;
                 }
                 else
                 {
                     trace.dropped = trace.dropped.saturating_add(1);
                 }
             });
    }
    pub(crate) fn finish()
    {
        if option_env!("SHELLY_PROFILE_STARTUP").is_none()
        {
            return;
        }
        mark("reedline_first_read");
        let trace = TRACE.with(|state| state.replace(Trace::new()));
        if trace.start.is_none()
        {
            return;
        }
        let capacity =
            trace.records[..trace.length].iter().try_fold(96usize, |bytes, record| {
                                                    bytes.checked_add(record.phase
                                                                            .len()
                                                                            .checked_add(96)?)
                                                });
        let Some(capacity) = capacity.filter(|bytes| *bytes <= 4096)
        else
        {
            emit("SHELLY_STARTUP unavailable=diagnostic-output-capacity\n");
            return;
        };
        let mut output = String::new();
        if output.try_reserve_exact(capacity).is_err()
        {
            emit("SHELLY_STARTUP unavailable=NoMemory\n");
            return;
        }
        for record in &trace.records[..trace.length]
        {
            let _ = writeln!(&mut output,
                             "SHELLY_STARTUP phase={} elapsed_us={}",
                             record.phase, record.micros);
        }
        if trace.dropped != 0
        {
            let _ = writeln!(&mut output, "SHELLY_STARTUP dropped={}", trace.dropped);
        }
        emit(&output);
    }
    fn emit(output: &str)
    {
        #[cfg(target_os = "systemrisc")]
        if let Ok(system) = mac_toolbox::System::new()
        {
            let _ = system.log(output);
        }
        #[cfg(not(target_os = "systemrisc"))]
        eprint!("{output}");
    }
}

#[derive(Parser, Debug)]
#[command(name = env!("CARGO_BIN_NAME"),
          version,
          about = "The Shelly interactive shell and scripting language.")]
struct CommandArguments
{
    /// Serve editor language features over the Language Server Protocol on stdio.
    #[arg(long, conflicts_with_all = ["interactive", "login", "code", "stdin_code",
                                     "rcfile", "script_arguments"])]
    language_server: bool,

    /// Force interactive mode.
    #[arg(short = 'i', conflicts_with_all = ["stdin_code"])]
    interactive: bool,

    /// Start as a login shell.
    #[arg(short = 'l', long)]
    login: bool,

    /// Execute the specified code and exit.
    #[arg(short = 'c', long,
          value_name = "code",
          conflicts_with_all = ["stdin_code", "interactive"])]
    code: Option<String>,

    /// Read source code from standard input; remaining arguments are passed to the script.
    #[arg(short = 's', long = "stdin")]
    stdin_code: bool,

    /// Do not show the shell's startup banner.
    #[arg(short = 'b', long)]
    no_banner: bool,

    /// Force monochrome mode.
    #[arg(short = 'm', long = "mono")]
    mono: bool,

    /// Specify the width that the shell should read tab characters as.
    #[arg(short = 't', long)]
    tab_width: Option<usize>,

    /// Do not load the shell's configuration file.
    #[arg(long)]
    norc: bool,

    /// Specify an alternative configuration file to load.
    #[arg(long, conflicts_with = "norc")]
    rcfile: Option<PathBuf>,

    /// Arguments passed to the script, ignored by the shell itself.
    #[arg(value_name = "script & arguments", trailing_var_arg = true, allow_hyphen_values = true)]
    script_arguments: Vec<String>
}


/**
 * The shell's execution mode, determined from command-line arguments and the system
 * environment.
 */
enum RunningMode
{
    /**
     * Are we running the full REPL?
     */
    Interactive,

    /**
     * Are we executing a script and then exiting?
     */
    Script(PathBuf),

    /**
     * Are we executing code passed via the command line and then exiting?
     */
    Code(String),

    /**
     * Are we reading source code from standard input and then exiting?
     */
    Stdin
}


/**
 * Based on the command line arguments and the terminal, (if any,) we're attached to determine
 * how the shell should run.
 */
fn determine_running_mode(args: &CommandArguments) -> (RunningMode, Vec<String>)
{
    // Did the user force interactive mode?
    if args.interactive
    {
        return (RunningMode::Interactive, args.script_arguments.clone());
    }

    // Or are we reading the source code for a script from standard input?
    if args.stdin_code
    {
        return (RunningMode::Stdin, args.script_arguments.clone());
    }

    // Or are we executing code passed via the command line?
    if args.code.is_some()
    {
        return (RunningMode::Code(args.code.as_ref().unwrap().clone()),
                args.script_arguments.clone());
    }

    // Or are we executing a script?
    //if args.script.is_some()
    //{
    //    return RunningMode::Script(args.script.as_ref().unwrap().clone());
    //}

    if args.script_arguments.len() >= 1
    {
        return (RunningMode::Script(PathBuf::from(&args.script_arguments[0])),
                args.script_arguments[1..].to_vec());
    }

    // Now determine if we're running in an interactive tty.
    #[cfg(target_os = "systemrisc")]
    let interactive = crate::runtime::terminal::attached();
    #[cfg(not(target_os = "systemrisc"))]
    let interactive = stdin().is_terminal() && stderr().is_terminal();
    if interactive
    {
        return (RunningMode::Interactive, args.script_arguments.clone());
    }

    // Default to reading from standard input.
    (RunningMode::Stdin, args.script_arguments.clone())
}


/**
 * Determine the color mode for the shell based on command-line arguments and environment.
 */
fn determine_color_mode(args: &CommandArguments) -> TtyColorMode
{
    if args.mono
    {
        return TtyColorMode::TtyMonochrome;
    }

    match on(Stream::Stdout)
    {
        Some(c) if c.has_16m => TtyColorMode::TtyTrueColor,
        Some(c) if c.has_256 => TtyColorMode::Tty256,
        Some(c) if c.has_basic => TtyColorMode::TtyBasic,
        _ => TtyColorMode::TtyMonochrome
    }
}


/**
 * Run the shell as a REPL, executing commands interactively.
 */
fn run_as_repl(tab_width: Option<usize>, color_mode: TtyColorMode, startup: Startup,
               suppress_banner: bool, rc_file: RcFile, script_args: Vec<String>)
               -> RuntimeResult<ExitCode>
{
    let _interrupts = InteractiveInterrupts::install().map_err(RuntimeError::SignalError)?;
    let mut repl = Repl::new(color_mode, tab_width, startup, suppress_banner, rc_file, script_args);

    repl.run()
}


/**
 * Interpret the source code from the given buffer using the interpreter.
 */
fn interpret(buffer: &mut dyn Buffer, startup: Startup, color_mode: TtyColorMode,
             tab_width: Option<usize>, script_args: Vec<String>)
             -> RuntimeResult<ExitCode>
{
    let _interrupts = InteractiveInterrupts::install().map_err(RuntimeError::SignalError)?;
    let mut interpreter = Interpreter::new(startup,
                                           Interactive::No,
                                           color_mode,
                                           tab_width,
                                           RcFile::None,
                                           script_args);

    if let Err(error) = interpreter.execute_from_buffer(buffer)
    {
        interpreter.report_uncaught(&error);
        if error.was_interrupted()
        {
            crate::runtime::signals::terminate_interrupted();
        }
        return Ok(ExitCode::from(1));
    }

    Ok(ExitCode::from(interpreter.exit_code))
}


/**
 * Run the script file as specified by the command-line arguments.
 */
fn run_script(script: &PathBuf, tab_width: Option<usize>, color_mode: TtyColorMode,
              startup: Startup, script_args: Vec<String>)
              -> RuntimeResult<ExitCode>
{
    let file = File::open(script);

    if let Err(e) = file
    {
        return Err(RuntimeError::FileOpenError(script.clone(), e));
    }

    let file = file.unwrap();
    let script = script.to_str().unwrap_or("input script");
    let mut file_buffer = BufReader::new(file);
    let mut buffer = ReadBuffer::new(&script, &mut file_buffer, tab_width);

    interpret(&mut buffer, startup, color_mode, tab_width, script_args)
}


/**
 * Run the code as specified in the command line arguments.
 */
fn run_code(code: &String, tab_width: Option<usize>, color_mode: TtyColorMode, startup: Startup,
            script_args: Vec<String>)
            -> RuntimeResult<ExitCode>
{
    let mut buffer = SimpleBuffer::new("command line", &code, tab_width);

    interpret(&mut buffer, startup, color_mode, tab_width, script_args)
}


/**
 * Run the code that's streaming in from stdin.
 */
fn run_stdin(tab_width: Option<usize>, color_mode: TtyColorMode, startup: Startup,
             script_args: Vec<String>)
             -> RuntimeResult<ExitCode>
{
    let mut buffer = BufReader::new(stdin());
    let mut buffer = ReadBuffer::new("standard input", &mut buffer, tab_width);

    interpret(&mut buffer, startup, color_mode, tab_width, script_args)
}


/**
 * Process the command line arguments and determine the mode we are running in.
 */
fn main() -> RuntimeResult<ExitCode>
{
    startup_trace::begin();
    let args = CommandArguments::parse();
    startup_trace::mark("arguments_ready");

    if let Some(tab_width) = args.tab_width
       && !(1..=32).contains(&tab_width)
    {
        return Err(RuntimeError::InvalidTabWidth);
    }

    if args.language_server
    {
        return Ok(match server::run()
        {
            Ok(code) => code,
            Err(error) =>
            {
                eprintln!("Language server error: {error}");
                ExitCode::FAILURE
            }
        });
    }

    let color_mode = determine_color_mode(&args);

    let invoked_as_login =
        args_os().next().is_some_and(|name| name.as_encoded_bytes().starts_with(b"-"));

    let rc_file = if let Some(custom_rc) = &args.rcfile
    {
        RcFile::Custom(custom_rc.clone())
    }
    else if args.norc
    {
        RcFile::None
    }
    else
    {
        RcFile::Default
    };

    let startup = if args.login || invoked_as_login { Startup::Login } else { Startup::NonLogin };

    let tab_width = args.tab_width;
    let (mode, script_args) = determine_running_mode(&args);
    #[cfg(target_os = "systemrisc")]
    let color_mode = if !args.mono
                        && matches!(mode, RunningMode::Interactive)
                        && crate::runtime::terminal::attached()
    {
        TtyColorMode::TtyTrueColor
    }
    else
    {
        color_mode
    };

    match mode
    {
        RunningMode::Interactive =>
        {
            run_as_repl(tab_width, color_mode, startup, args.no_banner, rc_file, script_args)
        }

        RunningMode::Script(path) => run_script(&path, tab_width, color_mode, startup, script_args),

        RunningMode::Code(code) => run_code(&code, tab_width, color_mode, startup, script_args),

        RunningMode::Stdin => run_stdin(tab_width, color_mode, startup, script_args)
    }
}
