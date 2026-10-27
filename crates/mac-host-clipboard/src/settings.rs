//! Versioned policy data only. Encoding is not a successful preference save;
//! resident storage must separately validate authority and commit a writable
//! transaction before reporting persistence.
use core::fmt;

use serde::{Deserialize, Serialize};

pub const SETTINGS_VERSION: u32 = 1;
pub const MAX_SETTINGS_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction
{
    #[default]
    Both,
    HostToGuest,
    GuestToHost
}
impl fmt::Display for Direction
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Both => "both directions",
                        Self::HostToGuest => "host-to-guest",
                        Self::GuestToHost => "guest-to-host"
                    })
    }
}
/// Validated immutable policy. There is deliberately no Deserialize impl on
/// this public type: every persisted document must pass `decode` validation.
///
/// ```compile_fail
/// let _ = serde_json_core::from_slice::<mac_host_clipboard::Settings>(b"{\"version\":999}");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings
{
    enabled:   bool,
    direction: Direction
}
impl Default for Settings
{
    fn default() -> Self
    {
        Self::new(true, Direction::Both)
    }
}
impl Settings
{
    pub const fn new(enabled: bool, direction: Direction) -> Self
    {
        Self { enabled,
               direction }
    }

    pub const fn enabled(self) -> bool
    {
        self.enabled
    }

    pub const fn direction(self) -> Direction
    {
        self.direction
    }

    pub const fn version(self) -> u32
    {
        SETTINGS_VERSION
    }

    pub const fn permits_import(self) -> bool
    {
        self.enabled && matches!(self.direction, Direction::Both | Direction::HostToGuest)
    }

    pub const fn permits_export(self) -> bool
    {
        self.enabled && matches!(self.direction, Direction::Both | Direction::GuestToHost)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SettingsError>
    {
        if bytes.len() > MAX_SETTINGS_BYTES
        {
            return Err(SettingsError::Size { bytes:   bytes.len(),
                                             maximum: MAX_SETTINGS_BYTES });
        }
        let mut unescape = [0; MAX_SETTINGS_BYTES];
        let (wire, _): (WireSettings, usize) =
            serde_json_core::from_slice_escaped(bytes, &mut unescape).map_err(SettingsError::Json)?;
        if wire.version != SETTINGS_VERSION
        {
            return Err(SettingsError::Version { observed: wire.version });
        }
        Ok(Self::new(wire.enabled, wire.direction.into()))
    }

    /// Writes a complete document or leaves caller storage untouched. This is
    /// bounded serialization only, never a disk commit or authority upgrade.
    pub fn encode(self, output: &mut [u8]) -> Result<usize, SettingsError>
    {
        let wire = WireSettings { version:   SETTINGS_VERSION,
                                  enabled:   self.enabled,
                                  direction: self.direction.into() };
        let mut staging = [0; MAX_SETTINGS_BYTES];
        let count =
            serde_json_core::to_slice(&wire, &mut staging).map_err(SettingsError::Encoding)?;
        if output.len() < count
        {
            return Err(SettingsError::Output { required:  count,
                                               available: output.len() });
        }
        output[..count].copy_from_slice(&staging[..count]);
        Ok(count)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSettings
{
    version:   u32,
    #[serde(default = "enabled_default")]
    enabled:   bool,
    #[serde(default)]
    direction: WireDirection
}
const fn enabled_default() -> bool
{
    true
}
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum WireDirection
{
    #[default]
    Both,
    HostToGuest,
    GuestToHost
}
impl From<WireDirection> for Direction
{
    fn from(value: WireDirection) -> Self
    {
        match value
        {
            WireDirection::Both => Self::Both,
            WireDirection::HostToGuest => Self::HostToGuest,
            WireDirection::GuestToHost => Self::GuestToHost
        }
    }
}
impl From<Direction> for WireDirection
{
    fn from(value: Direction) -> Self
    {
        match value
        {
            Direction::Both => Self::Both,
            Direction::HostToGuest => Self::HostToGuest,
            Direction::GuestToHost => Self::GuestToHost
        }
    }
}

#[derive(Debug)]
pub enum SettingsError
{
    Size
    {
        bytes:   usize,
        maximum: usize
    },
    Version
    {
        observed: u32
    },
    Json(serde_json_core::de::Error),
    Encoding(serde_json_core::ser::Error),
    Output
    {
        required:  usize,
        available: usize
    }
}
impl fmt::Display for SettingsError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str("Host Clipboard preferences ")?;
        match self
        {
            Self::Size { bytes,
                         maximum } =>
            {
                write!(f,
                       "need {bytes} bytes but the bounded policy document permits {maximum}; \
                        retain the active policy and recreate a smaller version 1 document")
            }
            Self::Version { observed } =>
            {
                write!(f,
                       "use unsupported version {observed}; this build requires version 1, so \
                        retain the active policy and rebuild or recreate the preference document")
            }
            Self::Json(cause) =>
            {
                write!(f,
                       "are invalid JSON: {cause}; require integer version 1, optional boolean \
                        enabled, and direction both, host-to-guest or guest-to-host; \
                        unknown/duplicate fields and trailing values are refused; retain the \
                        active policy and correct the document")
            }
            Self::Encoding(cause) =>
            {
                write!(f,
                       "could not encode the bounded version 1 document: {cause}; retain the \
                        current stored preferences and rebuild System")
            }
            Self::Output { required,
                           available } =>
            {
                write!(f,
                       "need {required} output bytes but only {available} are available; retain \
                        the current stored preferences and retry with a larger buffer")
            }
        }
    }
}
impl core::error::Error for SettingsError
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        match self
        {
            Self::Json(cause) => Some(cause),
            Self::Encoding(cause) => Some(cause),
            _ => None
        }
    }
}
