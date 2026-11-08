
use std::{ fmt::{ self, Debug, Formatter }, io::Error, path::PathBuf };

use crate::language::interpreter::InterpreterError;

pub enum RuntimeError
{
    InvalidTabWidth,
    FileOpenError(PathBuf, Error),
    SignalError(Error),
    InterpreterError(InterpreterError)
}


pub type RuntimeResult<T> = Result<T, RuntimeError>;


impl From<InterpreterError> for RuntimeError
{
    fn from(err: InterpreterError) -> Self
    {
        RuntimeError::InterpreterError(err)
    }
}


impl Debug for RuntimeError
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self
        {
            RuntimeError::InvalidTabWidth =>
            {
                write!(f, "Invalid tab width: expected an integer from 1 through 32.")
            }

            RuntimeError::FileOpenError(path, err) =>
            {
                write!(f, "Failed to open file {:?}: {}.", path, err)
            }

            RuntimeError::InterpreterError(err) =>
            {
                write!(f, "{:?}", err)
            }

            RuntimeError::SignalError(err) =>
            {
                write!(f, "Failed to install interactive signal handling: {}.", err)
            }
        }
    }
}
