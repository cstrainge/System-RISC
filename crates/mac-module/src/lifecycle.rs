//! MLIF v1: authenticated module startup and bounded diagnostics. This service
//! uses the existing broker; it does not change the native Host descriptor.
use crate::ClassId;
pub const CLASS: ClassId = ClassId(*b"MLIF");
pub const VERSION: u32 = 1;
pub const DEFER: u32 = 1;
pub const CONFIRM: u32 = 2;
pub const REFUSE: u32 = 3;
pub const REPORT: u32 = 4;
pub const STATUS: u32 = 5;
pub const MAX_CAUSE: usize = 240;
pub const REPLY_BYTES: usize = 256;
pub const SYNCHRONOUS: u32 = 0;
pub const PENDING: u32 = 1;
pub const ACTIVE: u32 = 2;
pub const REFUSED: u32 = 3;
