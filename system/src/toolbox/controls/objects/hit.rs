//! Same reverse-tree HIT sequence for trusted immediate and owned user work.
//! Only the latter copies snapshots; the trusted path retains its zero-copy walk.
use super::*;
struct HitItem<'a>
{
    id:    u64,
    state: &'a State,
    entry: DefinitionRef,
    body:  bool
}
struct OwnedItem
{
    id:    u64,
    state: State,
    entry: DefinitionRef,
    body:  bool
}
struct HitItems<'a>
{
    manager: &'a Manager,
    root:    u64,
    owner:   u64,
    point:   Point,
    current: Option<u64>
}
impl<'a> Iterator for HitItems<'a>
{
    type Item = Result<HitItem<'a>, Status>;

    fn next(&mut self) -> Option<Self::Item>
    {
        loop
        {
            let id = self.current?;
            let object = match self.manager.get(id, self.owner)
            {
                Ok(object) => object,
                Err(status) =>
                {
                    self.current = None;
                    return Some(Err(status));
                }
            };
            let body = id != self.root;
            self.current = if !body
            {
                None
            }
            else if object.tree.previous_sibling() != 0
            {
                let mut next = object.tree.previous_sibling();
                while let Some(object) = self.manager.objects.get(next)
                {
                    if object.tree.last_child() == 0
                    {
                        break;
                    }
                    next = object.tree.last_child();
                }
                Some(next)
            }
            else
            {
                Some(object.tree.parent)
            };
            if self.manager.visible_clip(id).is_none_or(|clip| clip.contains(self.point))
            {
                return Some(Ok(HitItem { id,
                                         state: &object.state,
                                         entry: object.definition.reference(),
                                         body }));
            }
        }
    }
}
pub(super) struct HitSnapshot
{
    point: Point,
    items: Vec<OwnedItem>
}
impl Manager
{
    fn hit_items(&self, root: u64, owner: u64, point: Point) -> Result<HitItems<'_>, Status>
    {
        self.table(root, owner)?;
        let mut id = root;
        while let Some(object) = self.objects.get(id)
        {
            if object.tree.last_child() == 0
            {
                break;
            }
            id = object.tree.last_child();
        }
        Ok(HitItems { manager: self,
                      root,
                      owner,
                      point,
                      current: Some(id) })
    }

    pub(crate) fn hit_children(&self, root: u64, owner: u64, point: Point, gate: TrapGate)
                               -> Result<(u64, i16), Status>
    {
        let items = self.hit_items(root, owner, point)?;
        let author = super::super::effects::Author::trusted(gate);
        // This Author cannot register a Pending: trusted-only immediate poll
        // finishes/refuses before a Manager borrow could escape Desktop.
        super::super::effects::immediate(hit_sequence(items, point, &author))
    }

    pub(super) fn snapshot_hit_children(&self, root: u64, owner: u64, point: Point)
                                        -> Result<HitSnapshot, Status>
    {
        let mut items = Vec::new();
        for item in self.hit_items(root, owner, point)?
        {
            let item = item?;
            items.try_reserve(1).map_err(|_| NO_MEMORY)?;
            items.push(OwnedItem { id:    item.id,
                                   state: item.state.clone(),
                                   entry: item.entry,
                                   body:  item.body });
        }
        Ok(HitSnapshot { point,
                         items })
    }
}
async fn hit_sequence<'a>(items: impl Iterator<Item = Result<HitItem<'a>, Status>>, point: Point,
                          author: &super::super::effects::Author)
                          -> Result<(u64, i16), Status>
{
    for item in items
    {
        let item = item?;
        if item.state.divider_hit_with(point, author).await?.is_some()
        {
            return Ok((item.id, SPLITTER_PART));
        }
        if let Some((_, part)) = item.state.scroll_hit_with(point, author).await?
        {
            return Ok((item.id, part));
        }
        if item.body
        {
            let part = item.state.hit_with(point, item.entry, author).await?;
            if part != 0 && part != 254
            {
                return Ok((item.id, part));
            }
        }
    }
    Ok((0, 0))
}
impl HitSnapshot
{
    pub(super) async fn calculate(self, author: &super::super::effects::Author)
                                  -> Result<(u64, i16), Status>
    {
        // Every State here belongs to this owned snapshot, not a Manager.
        let items = self.items.iter().map(|item| {
                                         Ok(HitItem { id:    item.id,
                                                      state: &item.state,
                                                      entry: item.entry,
                                                      body:  item.body })
                                     });
        hit_sequence(items, self.point, author).await
    }
}
