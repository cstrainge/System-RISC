//! Process identities are assigned by the scheduler, never supplied as authority.
use crate::*;
pub const CURRENT: u32 = 0;
pub const COUNT: u32 = 1;
pub const INDEX: u32 = 2;
pub const LAUNCH: u32 = 3;
pub const ACTIVATE: u32 = 4;
pub const HIDE: u32 = 5;
pub const HIDE_OTHERS: u32 = 6;
pub const SHOW_ALL: u32 = 7;
pub const EXIT: u32 = 8;
/// Size-distinct native termination. The result is a program's signed main/exit
/// code, never a native entry service Status. Accepted calls do not resume the
/// invoking author continuation; malformed or unauthenticated calls may refuse.
pub const NATIVE_EXIT_NORMAL: u32 = 0;
pub const NATIVE_EXIT_ABORT: u32 = 1;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeExitCall
{
    pub code:     i32,
    pub flags:    u32,
    pub reserved: u64
}
/// Exact mounted-volume/catalog identity, independent of path or creator code.
pub const IS_RUNNING: u32 = 9;
/// The genuine Finder main has committed its initial desktop. Release only its
/// future recovery allocation default; existing backing retains its origins.
/// All ProcessCall identity/input fields are zero; execution authenticates it.
pub const FINDER_DESKTOP_READY: u32 = 10;
/// Read the authenticated caller's startup stream attachments.
pub const STANDARD_STREAMS: u32 = 11;
/// Inspect a child retained by the authenticated launching process.
pub const CHILD_STATUS: u32 = 12;
/// Release the parent's result receipt without terminating the child.
pub const CHILD_FORGET: u32 = 13;
/// Return the caller's front Terminal window or inherited console session.
pub const CONSOLE_SESSION: u32 = 14;
pub const CHILD_RUNNING: u32 = 0;
pub const CHILD_EXITED: u32 = 1;
pub const CHILD_ABORTED: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ChildWaitCall
{
    pub child:    u64,
    pub state:    u32,
    pub code:     i32,
    pub reserved: [u64; 2]
}
/// Native lifecycle notification: payload packs volume low/CNID high; message
/// low bit reports whether this exact file still has a running instance.
pub const FILE_STATE_EVENT: u32 = 0x0c00_0000;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ProcessInfo
{
    pub id:          u64,
    pub thread:      u64,
    /// Readonly image alias identity; zero while image admission is incomplete.
    pub image:       u64,
    pub volume:      u32,
    pub cnid:        u32,
    pub foreground:  u32,
    pub hidden:      u32,
    pub name:        [u8; INLINE_STRING_BYTES],
    pub name_length: u32,
    pub switches:    u64
}
impl Default for ProcessInfo
{
    fn default() -> Self
    {
        Self { id:          0,
               thread:      0,
               image:       0,
               volume:      0,
               cnid:        0,
               foreground:  0,
               hidden:      0,
               name:        [0; INLINE_STRING_BYTES],
               name_length: 0,
               switches:    0 }
    }
}
impl ProcessInfo
{
    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ProcessCall
{
    pub operation:           u32,
    pub index:               u32,
    /// For a streamed LAUNCH, the Terminal session window.
    pub id:                  u64,
    pub volume:              u32,
    pub cnid:                u32,
    /// LAUNCH input or STANDARD_STREAMS reply, in stdin/stdout/stderr order.
    pub streams:             [u64; 3],
    /// Signed child result, valid with a terminal CHILD_STATUS index.
    pub exit_code:           i32,
    pub info:                ProcessInfo,
    /// Failure-only identity of a declared provider requirement. Zero otherwise.
    pub requirement_class:   [u8; 4],
    pub requirement_version: u32
}

/// Size-distinct native document delivery. Sender owns a preparing batch;
/// Commit transfers it atomically to the exact application's incoming queue.
pub const DOCUMENT_BEGIN: u32 = 0;
pub const DOCUMENT_PUSH: u32 = 1;
pub const DOCUMENT_COMMIT: u32 = 2;
pub const DOCUMENT_ABORT: u32 = 3;
pub const DOCUMENT_ACCEPTS: u32 = 4;
pub const DOCUMENT_NEXT: u32 = 5;
pub const DOCUMENT_AT: u32 = 6;
pub const DOCUMENT_FINISH: u32 = 7;
pub const DOCUMENT_ACCEPTS_DRAG: u32 = 8;
pub const DOCUMENT_AVAILABLE: u32 = 0x1000_0000;
pub const MAX_DOCUMENTS: usize = 1024;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DocumentCall
{
    pub drag:                u32,
    pub operation:           u32,
    pub handle:              u64,
    pub application_volume:  u32,
    pub application_cnid:    u32,
    pub volume:              u32,
    pub cnid:                u32,
    pub file_type:           [u8; 4],
    pub index:               u32,
    pub count:               u32,
    pub present:             u32,
    pub reserved:            [u32; 2],
    pub entry:               crate::files::CatalogInfo,
    pub process:             ProcessInfo,
    /// Failure-only declared provider identity from document launch. Zero otherwise.
    pub requirement_class:   [u8; 4],
    pub requirement_version: u32
}

#[cfg(test)]
mod document_layout_tests
{
    use super::*;
    #[test]
    fn native_document_ids_have_full_width_and_distinct_request_layout()
    {
        assert_eq!(core::mem::offset_of!(DocumentCall, handle), 8);
        assert_eq!(core::mem::offset_of!(DocumentCall, application_volume), 16);
        assert_ne!(core::mem::size_of::<DocumentCall>(), core::mem::size_of::<ProcessCall>());
        let call = DocumentCall { handle: u64::from(u32::MAX) + 1,
                                  ..Default::default() };
        assert!(call.handle > u64::from(u32::MAX));
    }
}

/// Execute inspection begins with the file identity. A handler may request
/// catalog metadata and receive it in a second call for the same file.
pub mod execution
{
    pub const CLASS: [u8; 4] = *b"EXEC";
    pub const VERSION: u32 = 1;
    pub const INSPECT: u32 = 1;
    pub const CATALOG_RESULT: u32 = 2;
    /// Identity, original fork query, signed status, then the bytes read.
    pub const FORK_RESULT: u32 = 3;
    /// Ask the claiming handler to select the child's offered endpoints.
    /// Request: executable identity followed by three u64 endpoint IDs.
    /// Reply: the three selected endpoint IDs, in stdio order.
    pub const ATTACH: u32 = 4;
    pub const STREAM_BYTES: usize = 24;
    pub const ATTACH_BYTES: usize = IDENTITY_BYTES + STREAM_BYTES;
    pub const IDENTITY_BYTES: usize = 8;
    pub const CATALOG_BYTES: usize = IDENTITY_BYTES + 8;
    pub const PASS: u8 = 0;
    pub const CONSOLE: u8 = 1;
    pub const QUERY_CATALOG: u8 = 2;
    /// Fork selector followed by little-endian u64 offset and u16 length.
    pub const QUERY_DATA: u8 = 3;
    pub const QUERY_RESOURCE: u8 = 4;
    pub const FORK_QUERY_BYTES: usize = 11;
    pub const FORK_HEADER_BYTES: usize = IDENTITY_BYTES + FORK_QUERY_BYTES + 4;
    pub const MAX_FORK_BYTES: usize = 4096;
    pub const MAX_QUERIES: usize = 8;
}

// Optional argv uses separate request sizes, preserving the ordinary PROCESS
// envelope for callers that do not pass or retrieve arguments.
pub const ARGUMENT_BYTES: usize = 4096;
pub const ARGUMENT_COUNT: usize = 128;

#[repr(C)]
pub struct LaunchArgvCall
{
    pub launch:      ProcessCall,
    /// Argument bytes followed by the optional UTF-8 executable search path.
    pub bytes:       *const u8,
    pub length:      u32,
    pub count:       u32,
    pub path_length: u32,
    pub reserved:    u32
}

#[repr(C)]
pub struct StartupArgvCall
{
    pub bytes:       *mut u8,
    pub capacity:    u32,
    pub length:      u32,
    pub count:       u32,
    /// Zero on input. Number of PATH bytes following the returned arguments.
    pub path_length: u32
}
