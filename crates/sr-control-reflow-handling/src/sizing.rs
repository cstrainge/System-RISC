use mac_abi::controls::reflow as wire;

use crate::{Dimensions, Effects, Error, Message, Outcome, Result, Size};

/// Optional defaults and optional overrides share a value type, but are stored
/// separately. A requested preferred size below minimum is retained verbatim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizePreferences
{
    pub minimum:   Option<Size>,
    pub preferred: Option<Size>
}
fn decode_optional(value: wire::OptionalDimensions) -> Result<Option<Size>>
{
    match value.present
    {
        0 if value.value == Dimensions::ZERO => Ok(None),
        1 => Ok(Some(value.value.try_into()?)),
        _ => Err(Error::InvalidEncoding)
    }
}
fn encode_optional(value: Option<Size>) -> wire::OptionalDimensions
{
    match value
    {
        None => wire::OptionalDimensions::default(),
        Some(value) =>
        {
            wire::OptionalDimensions { present: 1,
                                       value:   value.into() }
        }
    }
}
impl TryFrom<wire::SizePreferences> for SizePreferences
{
    type Error = Error;

    fn try_from(value: wire::SizePreferences) -> Result<Self>
    {
        Ok(Self { minimum:   decode_optional(value.minimum)?,
                  preferred: decode_optional(value.preferred)? })
    }
}
impl From<SizePreferences> for wire::SizePreferences
{
    fn from(value: SizePreferences) -> Self
    {
        Self { minimum:   encode_optional(value.minimum),
               preferred: encode_optional(value.preferred) }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Requirements
{
    minimum:   Size,
    preferred: Size
}
impl Requirements
{
    /// Normalize the effective answer, without mutating the requested value.
    pub fn new(minimum: Size, requested_preferred: Size) -> Self
    {
        Self { minimum,
               preferred: requested_preferred.max(minimum) }
    }

    pub const fn minimum(self) -> Size
    {
        self.minimum
    }

    pub const fn preferred(self) -> Size
    {
        self.preferred
    }
}
impl TryFrom<wire::Requirements> for Requirements
{
    type Error = Error;

    fn try_from(value: wire::Requirements) -> Result<Self>
    {
        let minimum: Size = value.minimum.try_into()?;
        let preferred: Size = value.preferred.try_into()?;
        if preferred.max(minimum) != preferred
        {
            return Err(Error::InvalidRequirements);
        }
        Ok(Self { minimum,
                  preferred })
    }
}
impl From<Requirements> for wire::Requirements
{
    fn from(value: Requirements) -> Self
    {
        Self { minimum:   value.minimum.into(),
               preferred: value.preferred.into() }
    }
}

/// The CDEF supplies constrained results, including wrapped-content height.
/// System keys cached results by constraints and content/style/tree revisions;
/// a stale measurement must not publish against changed dependencies. Neither
/// query nor this reply performs an assignment or chooses the child's position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement
{
    pub intrinsic:   Requirements,
    pub constrained: Requirements
}
impl TryFrom<wire::Measurement> for Measurement
{
    type Error = Error;

    fn try_from(value: wire::Measurement) -> Result<Self>
    {
        Ok(Self { intrinsic:   value.intrinsic.try_into()?,
                  constrained: value.constrained.try_into()? })
    }
}
impl From<Measurement> for wire::Measurement
{
    fn from(value: Measurement) -> Self
    {
        Self { intrinsic:   value.intrinsic.into(),
               constrained: value.constrained.into() }
    }
}

/// Embedded in resident-owned instance storage. Creation size is captured once;
/// assignment is deliberately separate and never overwrites this fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizingState
{
    creation:   Size,
    calculated: SizePreferences,
    overrides:  SizePreferences
}
impl SizingState
{
    pub fn new(creation: Size) -> Self
    {
        Self { creation,
               calculated: SizePreferences::default(),
               overrides: SizePreferences::default() }
    }

    pub const fn creation(&self) -> Size
    {
        self.creation
    }

    pub const fn calculated(&self) -> SizePreferences
    {
        self.calculated
    }

    pub const fn overrides(&self) -> SizePreferences
    {
        self.overrides
    }

    pub fn requirements(&self) -> Requirements
    {
        Requirements::new(self.overrides.minimum.or(self.calculated.minimum).unwrap_or(Size::ZERO),
                          self.overrides
                              .preferred
                              .or(self.calculated.preferred)
                              .unwrap_or(self.creation))
    }

    /// Apply explicit overrides to a current CDEF measurement without changing
    /// the calculated defaults or original creation fallback.
    pub fn resolve(&self, measured: Requirements) -> Requirements
    {
        Requirements::new(self.overrides.minimum.unwrap_or(measured.minimum()),
                          self.overrides.preferred.unwrap_or(measured.preferred()))
    }

    /// Shared settings dispatch, before publication. All sizes are already
    /// validated. The caller admits the returned effects before committing the
    /// prepared reply. Other operations retain their original payload for the
    /// CDEF or resident manager; they are not silently accepted here.
    pub fn prepare_message<O, R, S>(&mut self, message: Message<O, R, S>)
                                    -> Outcome<Message<O, R, S>, PreparedSettings<'_>, Error>
    {
        let prepared = match message
        {
            Message::SetMinimum(size) => self.prepare_minimum(Some(size)),
            Message::ClearMinimum => self.prepare_minimum(None),
            Message::SetPreferred(size) => self.prepare_preferred(Some(size)),
            Message::ClearPreferred => self.prepare_preferred(None),
            Message::SetSizePreferences(preferences) => self.prepare_preferences(preferences),
            other => return Outcome::Unhandled(other)
        };
        let effects = prepared.change().effects;
        Outcome::Handled { reply: prepared,
                           effects }
    }

    pub fn prepare_preferences(&mut self, preferences: SizePreferences) -> PreparedSettings<'_>
    {
        let next = Self { overrides: preferences,
                          ..*self };
        PreparedSettings::new(self, next)
    }

    pub fn prepare_minimum(&mut self, minimum: Option<Size>) -> PreparedSettings<'_>
    {
        self.prepare_preferences(SizePreferences { minimum,
                                                   ..self.overrides })
    }

    pub fn prepare_preferred(&mut self, preferred: Option<Size>) -> PreparedSettings<'_>
    {
        self.prepare_preferences(SizePreferences { preferred,
                                                   ..self.overrides })
    }

    /// Called only with current calculated defaults, after relevant dependencies
    /// have been measured. Does not clear explicit owner/parent overrides.
    pub fn prepare_calculated(&mut self, calculated: SizePreferences) -> PreparedSettings<'_>
    {
        let next = Self { calculated,
                          ..*self };
        PreparedSettings::new(self, next)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsChange
{
    pub overrides_changed:  bool,
    pub calculated_changed: bool,
    pub before:             Requirements,
    pub after:              Requirements,
    pub effects:            Effects
}

/// A scalar settings transaction, not a geometry/tree transaction. The exclusive
/// borrow prevents a stale sizing snapshot. Dropping it leaves state unchanged.
/// The resident caller must admit effect queue storage before commit, then apply
/// effects and any infallible notification. No callback, allocation, lock,
/// drawing or recursive layout is performed here. Do not hold this borrow across
/// child-service callbacks; those need the resident generation/pinning protocol.
#[must_use = "admit effects and commit, or drop to leave sizing unchanged"]
pub struct PreparedSettings<'a>
{
    state:  &'a mut SizingState,
    next:   SizingState,
    change: SettingsChange
}
impl<'a> PreparedSettings<'a>
{
    fn new(state: &'a mut SizingState, next: SizingState) -> Self
    {
        let before = state.requirements();
        let after = next.requirements();
        let changed = before != after;
        let change = SettingsChange { overrides_changed: state.overrides != next.overrides,
                                      calculated_changed: state.calculated != next.calculated,
                                      before,
                                      after,
                                      effects: Effects { measurement: changed,
                                                         arrangement: changed,
                                                         ..Effects::default() } };
        Self { state,
               next,
               change }
    }

    pub const fn change(&self) -> SettingsChange
    {
        self.change
    }

    /// No fallible work remains in this scalar publication.
    pub fn commit(self) -> SettingsChange
    {
        if self.change.overrides_changed || self.change.calculated_changed
        {
            *self.state = self.next;
        }
        self.change
    }
}
