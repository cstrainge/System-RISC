use crate::language::{ data::value::Value,
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       text::location::Location };


// Native implementation of the file-test predicates behind the prelude's
// dir_exists/file_exists/is_pipe/is_tty. Same flags as the external test
// command, none of the fork+exec.
impl Interpreter
{
    pub(super) fn handle_test(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let mismatch = |message: String|
        {
            InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(message))
        };
        let [flag, target] = args
        else
        {
            return Err(mismatch("__test expects a flag and a target".into()));
        };
        let Value::String(flag, _) = flag
        else
        {
            return Err(mismatch("__test flag must be a string".into()));
        };
        let result = match flag.as_str()
        {
            "-d" | "-f" | "-e" =>
            {
                let Value::String(path, _) = target
                else
                {
                    return Err(mismatch(format!("__test {flag} expects a path string")));
                };
                // Variable reads shorten home paths; resolve them as other filesystem APIs do.
                // metadata follows symlinks, matching test -d/-f/-e.
                match std::fs::metadata(self.eval_path_from(path))
                {
                    Ok(metadata) => match flag.as_str()
                    {
                        "-d" => metadata.is_dir(),
                        "-f" => metadata.is_file(),
                        _ => true,
                    },
                    Err(_) => false,
                }
            },
            "-p" | "-t" =>
            {
                let Value::Integer(fd) = target
                else
                {
                    return Err(mismatch(format!("__test {flag} expects a file descriptor integer")));
                };
                #[cfg(unix)]
                {
                    if *fd < 0 || *fd > i32::MAX as i64
                    {
                        false
                    }
                    else if flag.as_str() == "-t"
                    {
                        unsafe { libc::isatty(*fd as libc::c_int) == 1 }
                    }
                    else
                    {
                        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                        let ok = unsafe { libc::fstat(*fd as libc::c_int, &mut stat) } == 0;
                        ok && (stat.st_mode & libc::S_IFMT) == libc::S_IFIFO
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = fd;
                    false
                }
            },
            _ => return Err(mismatch(format!("__test does not support flag '{flag}'"))),
        };
        self.last_result = Some(Value::Boolean(result));
        Ok(())
    }
}
