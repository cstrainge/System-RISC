//! Rust drawing targets and their checked native wire projection. Object tokens
//! are opaque; no target kind is encoded into their bits.
use crate::WindowId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct WorldId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawingTarget
{
    Window(WindowId),
    Desktop,
    World(WorldId)
}
impl From<WindowId> for DrawingTarget
{
    fn from(window: WindowId) -> Self
    {
        Self::Window(window)
    }
}
impl From<WorldId> for DrawingTarget
{
    fn from(world: WorldId) -> Self
    {
        Self::World(world)
    }
}

pub const TARGET_WINDOW: u32 = 0;
pub const TARGET_DESKTOP: u32 = 1;
pub const TARGET_WORLD: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Target
{
    pub kind:     u32,
    pub reserved: u32,
    pub id:       u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetError
{
    UnknownKind(u32),
    Reserved(u32),
    DesktopIdentity(u64),
    WindowIdentity(u64)
}
impl core::fmt::Display for TargetError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::UnknownKind(kind) => write!(f, "unknown drawing target kind {kind}"),
            Self::Reserved(value) =>
            {
                write!(f, "drawing target reserved field must be zero, got {value}")
            }
            Self::DesktopIdentity(id) =>
            {
                write!(f, "desktop target cannot carry object identity {id}")
            }
            Self::WindowIdentity(id) =>
            {
                write!(f, "window identity {id} cannot be represented by this Window Manager")
            }
        }
    }
}
impl core::error::Error for TargetError
{
}
impl Target
{
    pub const fn window(window: WindowId) -> Self
    {
        Self { kind:     TARGET_WINDOW,
               reserved: 0,
               id:       window.0 as u64 }
    }

    pub const fn desktop() -> Self
    {
        Self { kind:     TARGET_DESKTOP,
               reserved: 0,
               id:       0 }
    }

    pub const fn world(world: WorldId) -> Self
    {
        Self { kind:     TARGET_WORLD,
               reserved: 0,
               id:       world.0 }
    }

    pub fn decode(self) -> Result<DrawingTarget, TargetError>
    {
        if self.reserved != 0
        {
            return Err(TargetError::Reserved(self.reserved));
        }
        match self.kind
        {
            TARGET_WINDOW =>
            {
                Ok(DrawingTarget::Window(WindowId(u32::try_from(self.id).map_err(|_| {
                                                      TargetError::WindowIdentity(self.id)
                                                  })?)))
            }
            TARGET_DESKTOP if self.id == 0 => Ok(DrawingTarget::Desktop),
            TARGET_DESKTOP => Err(TargetError::DesktopIdentity(self.id)),
            TARGET_WORLD => Ok(DrawingTarget::World(WorldId(self.id))),
            other => Err(TargetError::UnknownKind(other))
        }
    }
}
impl From<DrawingTarget> for Target
{
    fn from(target: DrawingTarget) -> Self
    {
        match target
        {
            DrawingTarget::Window(window) => Self::window(window),
            DrawingTarget::Desktop => Self::desktop(),
            DrawingTarget::World(world) => Self::world(world)
        }
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn target_projection_is_explicit_and_checked()
    {
        for target in [DrawingTarget::Desktop,
                       DrawingTarget::Window(WindowId(0x80000000)),
                       DrawingTarget::World(WorldId(u64::MAX))]
        {
            assert_eq!(Target::from(target).decode(), Ok(target));
        }
        assert_eq!(Target { kind: 3,
                            ..Default::default() }.decode(),
                   Err(TargetError::UnknownKind(3)));
        assert_eq!(Target { reserved: 1,
                            ..Default::default() }.decode(),
                   Err(TargetError::Reserved(1)));
        assert_eq!(Target { id: 1,
                            ..Target::desktop() }.decode(),
                   Err(TargetError::DesktopIdentity(1)));
        assert_eq!(Target { id: u64::MAX,
                            ..Default::default() }.decode(),
                   Err(TargetError::WindowIdentity(u64::MAX)));
    }
}
