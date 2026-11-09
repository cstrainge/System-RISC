# DateTime control definition

System CDEF 41, classic procedure 656, ABI 1.

This v2 stub draws only a theme-role rectangle and X. Typed instance
properties are shared with resident storage and the public SDK; it participates
in leaf reflow. Unimplemented behavior deliberately ignores events and emits
none. It does not implement the final control's chrome or interactions.

Unspecified size defaults are explicit placeholders: minimum 1 by 1, no
application maximum, creation size preferred. Other placeholder defaults are
spelled out in the shared property model. Theme properties describe theme
policy, not application-selected colors. Composed child/source identities are
state only; the stub does not turn them into capabilities or run providers.

Properties: `visible`, `voice`, `tooltip`, `drag_drop`, `minimum`, `maximum`, `mode`, `value`, `month`, `day`, `year`, `year_range`, `hour`, `minute`, `ampm`, `timezone`, `month_menu`, `day_menu`, `year_menu`, `hour_editor`, `minute_editor`, `ampm_menu`, `timezone_menu`, plus resident
`bounds` and `enabled`. Use the typed public control owner and properties API.
