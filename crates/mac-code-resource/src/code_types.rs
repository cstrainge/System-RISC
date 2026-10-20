//! Executable resource families and their calling contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallingConvention
{
    WindowDefinition,
    ControlDefinition,
    MenuDefinition,
    MenuBarDefinition,
    ListDefinition,
    CodeSegment,
    CodeJumpTable,
    Initializer,
    Driver,
    FunctionKey,
    ControlPanel,
    Package,
    Procedure,
    Patch,
    Decompressor,
    AdbDriver,
    Custom
}

pub const STANDARD_CODE_TYPES: &[([u8; 4], CallingConvention)] =
    &[(*b"WDEF", CallingConvention::WindowDefinition),
      (*b"CDEF", CallingConvention::ControlDefinition),
      (*b"MDEF", CallingConvention::MenuDefinition),
      (*b"MBDF", CallingConvention::MenuBarDefinition),
      (*b"LDEF", CallingConvention::ListDefinition),
      (*b"CODE", CallingConvention::CodeSegment),
      (*b"INIT", CallingConvention::Initializer),
      (*b"DRVR", CallingConvention::Driver),
      (*b"FKEY", CallingConvention::FunctionKey),
      (*b"cdev", CallingConvention::ControlPanel),
      (*b"PACK", CallingConvention::Package),
      (*b"proc", CallingConvention::Procedure),
      (*b"ptch", CallingConvention::Patch),
      (*b"dcmp", CallingConvention::Decompressor),
      (*b"ADBS", CallingConvention::AdbDriver)];
