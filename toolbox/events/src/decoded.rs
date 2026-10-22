//! Decoded native events. Classic EventRecords remain in `classic`.
use mac_abi::{Point, WINDOW_CLOSE_REQUEST, WindowId, events as raw};

macro_rules! flags {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $name(u16);
        impl $name
        {
            pub const fn from_bits_retain(bits: u16) -> Self
            {
                Self(bits)
            }

            pub const fn bits(self) -> u16
            {
                self.0
            }

            pub const fn contains(self, other: Self) -> bool
            {
                self.0 & other.0 == other.0
            }

            pub const fn intersects(self, other: Self) -> bool
            {
                self.0 & other.0 != 0
            }
        }
        impl core::ops::BitOr for $name
        {
            type Output = Self;

            fn bitor(self, rhs: Self) -> Self
            {
                Self(self.0 | rhs.0)
            }
        }
        impl core::ops::BitAnd for $name
        {
            type Output = Self;

            fn bitand(self, rhs: Self) -> Self
            {
                Self(self.0 & rhs.0)
            }
        }
    };
}
flags!(Modifiers);
impl Modifiers
{
    pub const ACTIVE: Self = Self(raw::ACTIVE_FLAG);
    pub const CAPS_LOCK: Self = Self(raw::ALPHA_LOCK);
    pub const COMMAND: Self = Self(raw::CMD_KEY);
    pub const CONTROL: Self = Self(raw::CONTROL_KEY);
    pub const OPTION: Self = Self(raw::OPTION_KEY);
    pub const SHIFT: Self = Self(raw::SHIFT_KEY);

    /// The classic button-state flag is set when the primary button is released.
    pub const fn button_down(self) -> bool
    {
        self.0 & raw::BTN_STATE == 0
    }
}
flags!(EventMask);
impl EventMask
{
    pub const ACTIVATE: Self = Self(1 << raw::EventKind::Activate as u16);
    pub const ALL: Self = Self(u16::MAX);
    pub const AUTO_KEY: Self = Self(1 << raw::EventKind::AutoKey as u16);
    pub const KEY_DOWN: Self = Self(1 << raw::EventKind::KeyDown as u16);
    pub const KEY_UP: Self = Self(1 << raw::EventKind::KeyUp as u16);
    pub const MENU_COMMAND: Self = Self(1 << raw::EventKind::MenuCommand as u16);
    pub const MOUSE_DOWN: Self = Self(1 << raw::EventKind::MouseDown as u16);
    pub const MOUSE_UP: Self = Self(1 << raw::EventKind::MouseUp as u16);
    pub const NONE: Self = Self(0);
    pub const SYSTEM: Self = Self(1 << raw::EventKind::OsEvent as u16);
    pub const UPDATE: Self = Self(1 << raw::EventKind::Update as u16);
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input
{
    pub at:        Point,
    pub when:      u32,
    pub modifiers: Modifiers
}
/// A decoded key; the System currently delivers System 7 MacRoman characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key
{
    message: u32
}
impl Key
{
    /// Construct synthetic keyboard input using the current resident keyboard
    /// encoding. Unsupported Unicode is rejected rather than substituted.
    pub fn new(character: char, code: u8) -> mac_traps::Result<Self>
    {
        let byte = if character.is_ascii()
        {
            character as u8
        }
        else
        {
            mac_text::HIGH.iter()
                          .position(|&c| c == character)
                          .map(|index| index as u8 + 128)
                          .ok_or(mac_traps::Error::BadArgument)?
        };
        Ok(Self { message: (u32::from(code) << 8) | u32::from(byte) })
    }

    pub fn character(self) -> char
    {
        mac_text::character(self.message as u8)
    }

    pub const fn code(self) -> u8
    {
        (self.message >> 8) as u8
    }
}
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a>
{
    /// Borrowed metadata or drop capabilities from the resident Drag Manager.
    Drag
    {
        offer: toolbox_drag::DragEvent<'a>, input: Input
    },
    /// No queued event, including a pointer-motion wake. Retains physical input
    /// for drag/caret work without another trap. Poll and wait agree on this.
    Idle
    {
        input: Input, tag: u32
    },
    MouseDown
    {
        window: Option<WindowId>, input: Input
    },
    MouseUp
    {
        window: Option<WindowId>, input: Input
    },
    KeyDown
    {
        key: Key, repeat: bool, input: Input
    },
    KeyUp
    {
        key: Key, input: Input
    },
    Update
    {
        window: WindowId, input: Input
    },
    Activate
    {
        window: WindowId, active: bool, input: Input
    },
    MenuCommand
    {
        menu: mac_traps::MenuId, item: u16, input: Input
    },
    CloseRequested
    {
        window: WindowId, input: Input
    },
    /// Application-defined/classic events with no native payload contract.
    Other
    {
        kind: raw::EventKind, message: u32, input: Input
    }
}
impl Default for Event<'_>
{
    fn default() -> Self
    {
        Self::Idle { input: Input::default(),
                     tag:   0 }
    }
}
impl<'a> From<raw::Event> for Event<'a>
{
    fn from(event: raw::Event) -> Self
    {
        let input = Input { at:        event.where_,
                            when:      event.when,
                            modifiers: Modifiers::from_bits_retain(event.modifiers) };
        let window = WindowId(event.message);
        match event.kind
        {
            raw::EventKind::Null =>
            {
                Self::Idle { input,
                             tag: event.message }
            }
            raw::EventKind::MouseDown =>
            {
                Self::MouseDown { window: (window.0 != 0).then_some(window),
                                  input }
            }
            raw::EventKind::MouseUp =>
            {
                Self::MouseUp { window: (window.0 != 0).then_some(window),
                                input }
            }
            raw::EventKind::KeyDown | raw::EventKind::AutoKey =>
            {
                Self::KeyDown { key: Key { message: event.message },
                                repeat: event.kind == raw::EventKind::AutoKey,
                                input }
            }
            raw::EventKind::KeyUp =>
            {
                Self::KeyUp { key: Key { message: event.message },
                              input }
            }
            raw::EventKind::Update =>
            {
                Self::Update { window,
                               input }
            }
            raw::EventKind::Activate =>
            {
                Self::Activate { window,
                                 active: input.modifiers.contains(Modifiers::ACTIVE),
                                 input }
            }
            raw::EventKind::MenuCommand =>
            {
                Self::MenuCommand { menu: mac_traps::MenuId((event.message >> 16) as i16),
                                    item: event.message as u16,
                                    input }
            }
            raw::EventKind::OsEvent if event.message & 0xff00_0000 == WINDOW_CLOSE_REQUEST =>
            {
                Self::CloseRequested { window: WindowId(event.message & 0x00ff_ffff),
                                       input }
            }
            kind =>
            {
                Self::Other { kind,
                              message: event.message,
                              input }
            }
        }
    }
}
impl Event<'_>
{
    pub const fn input(self) -> Input
    {
        match self
        {
            Self::Drag { input,
                         .. } => input,
            Self::Idle { input,
                         .. }
            | Self::MouseDown { input,
                              .. }
            | Self::MouseUp { input,
                            .. }
            | Self::KeyDown { input,
                            .. }
            | Self::KeyUp { input,
                          .. }
            | Self::Update { input,
                           .. }
            | Self::Activate { input,
                             .. }
            | Self::MenuCommand { input,
                                .. }
            | Self::CloseRequested { input,
                                   .. }
            | Self::Other { input,
                          .. } => input
        }
    }

    pub const fn key(self) -> Option<Key>
    {
        match self
        {
            Self::KeyDown { key,
                            .. }
            | Self::KeyUp { key,
                          .. } => Some(key),
            _ => None
        }
    }

    pub const fn is_idle(self) -> bool
    {
        matches!(self, Self::Idle { .. })
    }

    /// Explicit transport conversion, for classic callbacks and diagnostics.
    pub fn into_record(self) -> raw::Event
    {
        let input = self.input();
        if let Self::Drag { offer,
                            .. } = self
        {
            return offer.into_record(input.when, input.modifiers.bits());
        }
        let (kind, message) = match self
        {
            Self::Drag { .. } => unreachable!(),
            Self::Idle { tag,
                         .. } => (raw::EventKind::Null, tag),
            Self::MouseDown { window,
                              .. } => (raw::EventKind::MouseDown, window.map_or(0, |id| id.0)),
            Self::MouseUp { window,
                            .. } => (raw::EventKind::MouseUp, window.map_or(0, |id| id.0)),
            Self::KeyDown { key,
                            repeat,
                            .. } =>
            {
                (if repeat { raw::EventKind::AutoKey } else { raw::EventKind::KeyDown },
                 key.message)
            }
            Self::KeyUp { key,
                          .. } => (raw::EventKind::KeyUp, key.message),
            Self::Update { window,
                           .. } => (raw::EventKind::Update, window.0),
            Self::Activate { window,
                             active,
                             .. } =>
            {
                let mut record = raw::Event { kind:      raw::EventKind::Activate,
                                              message:   window.0,
                                              where_:    input.at,
                                              when:      input.when,
                                              modifiers: input.modifiers.bits(),
                                              payload:   0 };
                record.modifiers = (record.modifiers & !raw::ACTIVE_FLAG) | u16::from(active);
                return record;
            }
            Self::MenuCommand { menu,
                                item,
                                .. } =>
            {
                (raw::EventKind::MenuCommand, ((menu.0 as u16 as u32) << 16) | item as u32)
            }
            Self::CloseRequested { window,
                                   .. } =>
            {
                (raw::EventKind::OsEvent, WINDOW_CLOSE_REQUEST | window.0)
            }
            Self::Other { kind,
                          message,
                          .. } => (kind, message)
        };
        raw::Event { kind,
                     message,
                     when: input.when,
                     where_: input.at,
                     modifiers: input.modifiers.bits(),
                     payload: 0 }
    }
}
impl Event<'_>
{
    pub const fn window(self) -> Option<WindowId>
    {
        match self
        {
            Self::MouseDown { window,
                              .. }
            | Self::MouseUp { window,
                            .. } => window,
            Self::Drag { offer,
                         .. } => offer.window(),
            Self::Update { window,
                           .. }
            | Self::Activate { window,
                             .. }
            | Self::CloseRequested { window,
                                   .. } => Some(window),
            _ => None
        }
    }
}
impl From<Event<'_>> for raw::Event
{
    fn from(event: Event<'_>) -> Self
    {
        event.into_record()
    }
}

impl<'a> Event<'a>
{
    pub(crate) fn decode(client: mac_traps::Client<'a>, record: raw::Event) -> Self
    {
        if record.kind == raw::EventKind::OsEvent
           && let Some(offer) = toolbox_drag::DragEvent::decode(client, record)
        {
            return Self::Drag { offer,
                                input: Input { at:        record.where_,
                                               when:      record.when,
                                               modifiers:
                                                   Modifiers::from_bits_retain(record.modifiers) } };
        }
        Self::from(record)
    }
}
