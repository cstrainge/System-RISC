//! Desktop Apple menu. App rows retain their generation-checked command owner.
use super::*;
pub(super) const MAX_APP_ITEMS: usize = MAX_ITEMS - 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action
{
    AboutSystem,
    Application
    {
        handle:    u64,
        selection: Selection
    }
}

pub(crate) struct SystemBar
{
    template:         Box<MenuDefinitionState>,
    pub(super) state: Box<MenuDefinitionState>,
    scratch:          Box<MenuDefinitionState>,
    rows:             [Option<Action>; MAX_ITEMS],
    pub(super) owner: u64,
    source:           Option<(u64, u64)>,
    entry:            DefinitionEntry,
    gate:             TrapGate
}
impl SystemBar
{
    pub(crate) fn new(entry: DefinitionEntry, gate: TrapGate, resources: Resources<'_>)
                      -> Result<Self, Status>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::with_system_allocation(|| {
            let mut template = new_state()?;
            let (_, resource) = resources.get(resources::SYSTEM,
                                              ResourceKey { kind: *b"MENU",
                                                            id:   SYSTEM_APPLE_MENU_ID })?;
            parse_menu(&resource.data, &mut template)?;
            let (_, commands) = resources.get(resources::SYSTEM,
                                              ResourceKey { kind: *b"MCMD",
                                                            id:   SYSTEM_APPLE_MENU_ID })?;
            if commands.data.as_slice() != b"asys"
            {
                return Err(BAD_ARGUMENT);
            }
            template.items[0].command = u32::from_be_bytes(*b"asys");
            if !is_apple(&template)
               || template.item_count != 1
               || template.items[0].command != u32::from_be_bytes(*b"asys")
            {
                return Err(BAD_ARGUMENT);
            }
            template.title_bounds.left = 10;
            Manager::measure_with(entry, gate, &mut template)?;
            let mut state = new_state()?;
            state.as_mut().clone_from(template.as_ref());
            Ok(Self { template,
                      state,
                      scratch: new_state()?,
                      rows: [None; MAX_ITEMS],
                      owner: 0,
                      source: None,
                      entry,
                      gate })
        })
    }

    pub(crate) fn title_bounds(&self) -> Rect
    {
        self.template.title_bounds
    }

    pub(crate) fn reset(&mut self, owner: u64)
    {
        self.state.as_mut().clone_from(self.template.as_ref());
        self.rows.fill(None);
        self.rows[0] = Some(Action::AboutSystem);
        self.owner = owner;
        self.source = None;
    }

    pub(super) fn refresh(&mut self, owner: u64, menu: Option<&Menu>) -> Result<bool, Status>
    {
        let source = menu.map(|menu| (menu.id, menu.revision));
        if self.owner == owner && self.source == source
        {
            return Ok(false);
        }
        if menu.is_none()
        {
            self.reset(owner);
            return Ok(true);
        }
        let state = &mut *self.scratch;
        state.clone_from(self.template.as_ref());
        state.items.fill(MenuItemState::EMPTY);
        state.item_count = 0;
        let mut rows = [None; MAX_ITEMS];
        if let Some(menu) = menu
        {
            if menu.state.item_count as usize > MAX_APP_ITEMS
            {
                return Err(NO_MEMORY);
            }
            let items = &menu.state.items[..menu.state.item_count as usize];
            let first = items.iter().position(|item| item.separator == 0);
            let last = items.iter().rposition(|item| item.separator == 0);
            if let (Some(first), Some(last)) = (first, last)
            {
                for (index, item) in items.iter().enumerate().take(last + 1).skip(first)
                {
                    if item.separator != 0
                       && state.item_count != 0
                       && state.items[state.item_count as usize - 1].separator != 0
                    {
                        continue;
                    }
                    let at = state.item_count as usize;
                    state.items[at] = *item;
                    state.items[at].enabled &= menu.state.enabled;
                    if item.separator == 0
                    {
                        rows[at] = Some(Action::Application { handle:    menu.id,
                                                              selection:
                                                                  Selection { menu_id: menu.state
                                                                                           .menu_id,
                                                                              item:    index as u16
                                                                                       + 1,
                                                                              command: item.command } });
                    }
                    state.item_count += 1;
                }
                state.items[state.item_count as usize].separator = 1;
                state.items[state.item_count as usize].enabled = 0;
                state.item_count += 1;
            }
        }
        let at = state.item_count as usize;
        state.items[at] = self.template.items[0];
        rows[at] = Some(Action::AboutSystem);
        state.item_count += 1;
        Manager::measure_with(self.entry, self.gate, state)?;
        core::mem::swap(&mut self.state, &mut self.scratch);
        self.rows = rows;
        self.owner = owner;
        self.source = source;
        Ok(true)
    }

    pub(super) fn apply_layout(&self, app: &mut MenuDefinitionState)
    {
        app.title_bounds = self.state.title_bounds;
        app.popup_bounds = self.state.popup_bounds;
        for (row, action) in self.state.items.iter().zip(self.rows)
        {
            if let Some(Action::Application { selection,
                                              .. }) = action
            {
                app.items[selection.item as usize - 1].bounds = row.bounds;
            }
        }
    }

    pub(crate) fn query(&self, call: &mut MenuCall) -> Result<(), Status>
    {
        match call.operation
        {
            LAYOUT =>
            {
                call.title_bounds = self.state.title_bounds;
                call.popup_bounds = self.state.popup_bounds;
            }
            COMMAND if call.item == 0 =>
            {
                call.value = self.state.item_count;
                call.command = 0;
            }
            COMMAND =>
            {
                let at = usize::from(call.item) - 1;
                let row = self.state
                              .items
                              .get(at)
                              .filter(|_| at < self.state.item_count as usize)
                              .ok_or(NOT_FOUND)?;
                call.command = row.command;
                call.value = u32::from(row.enabled != 0 && row.separator == 0)
                             | (u32::from(row.checked != 0) << 1);
            }
            _ => return Err(ACCESS_DENIED)
        }
        Ok(())
    }

    pub(super) fn action(&self, item: i16) -> Option<Action>
    {
        let at = usize::try_from(item).ok()?.checked_sub(1)?;
        let row = self.state.items.get(at)?;
        if at >= self.state.item_count as usize || row.enabled == 0 || row.separator != 0
        {
            return None;
        }
        self.rows[at]
    }

    pub(super) fn key_item(&self, selection: Selection) -> Option<i16>
    {
        let at = self.rows.iter().enumerate().find_map(|(at, action)| {
                                                  match action
                                                  {
                                                      Some(Action::Application { selection:
                                                                                     current,
                                                                                 .. })
                                                          if *current == selection =>
                                                      {
                                                          Some(at)
                                                      }
                                                      _ => None
                                                  }
                                              })?;
        Some(at as i16 + 1)
    }
}
