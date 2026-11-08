#[cfg(all(unix, feature = "event-stream"))]
pub(crate) use unix::waker::Waker;
#[cfg(all(windows, feature = "event-stream"))]
pub(crate) use windows::waker::Waker;

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(target_os = "systemrisc")]
#[path = "sys/unix/parse.rs"]
pub(crate) mod native_parse;
#[cfg(windows)]
pub(crate) mod windows;
