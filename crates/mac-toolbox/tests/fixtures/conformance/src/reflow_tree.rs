//! Existing disk-loaded native fixture: real CTRL ownership and recovery paths.
use mac_toolbox::{controls::{self, Cell, Reflow, StandardKind, StandardState},
                  *};

use super::conformance::Failure;

fn require(ok: bool, message: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn pool(system: &System<'_>) -> Result<inspection::Pool, Failure>
{
    let pools = system.inspection()
                      .snapshot(inspection::Scope::Own)
                      .map_err(|e| e.cause)?
                      .ledger(0)
                      .map_err(|e| e.cause)?;
    for index in 0..pools.len()
    {
        let pool = pools.at(index).map_err(|e| e.cause)?;
        if pool.kind == *b"CTRL"
        {
            return Ok(pool);
        }
    }
    Err(Failure::Mismatch("control ledger absent before tree scenario"))
}
fn adopt<'a>(table: controls::TableRef<'a>, cell: Cell, child: controls::Control<'a>)
             -> Result<controls::ControlRef<'a>, Failure>
{
    table.set_child(cell, child).map_err(|error| Failure::Service(error.cause.status()))
}
fn button<'a>(system: &System<'a>, window: WindowId, title: &str)
              -> Result<controls::Control<'a>, Failure>
{
    Ok(system.controls().create_standard(window,
                                          StandardKind::Button,
                                          StandardState::new(Rect::new(16, 16, 116, 44), title),
                                          Reflow::Yes)?)
}
pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let before = pool(system)?;
    let window =
        system.windows().create_owned(windows::WindowSpec::new(Rect::new(70, 70, 390, 290),
                                                                "Table ownership"))?;
    window.select()?;
    let table =
        system.controls().create_table(window.id(), Rect::new(8, 8, 240, 160), Reflow::Yes)?;
    let parent = table.table();
    let other =
        system.controls().create_table(window.id(), Rect::new(8, 8, 240, 160), Reflow::Yes)?;
    let first = Cell { row:    0,
                       column: 0 };
    let second = Cell { row:    0,
                        column: 1 };
    let front = adopt(parent, second, button(system, window.id(), "Front cell")?)?;
    let back = adopt(parent, first, button(system, window.id(), "Back cell")?)?;
    require(front.info()?.parent == Some(parent.control().id()),
            "adoption did not publish parent")?;
    let mut children = parent.children();
    require(children.next().ok_or(Failure::Mismatch("missing first cell"))??.control.id()
            == back.id(),
            "grid enumeration not row-major")?;
    require(children.next().ok_or(Failure::Mismatch("missing second cell"))??.control.id()
            == front.id(),
            "grid second cell wrong")?;
    require(children.next().is_none(), "grid enumerated extra child")?;

    let rejected = parent.set_child(first, button(system, window.id(), "Collision")?)
                         .err()
                         .ok_or(Failure::Mismatch("occupied cell accepted"))?;
    require(rejected.cause == Error::AlreadyExists, "wrong collision cause")?;
    require(rejected.child.control().info()?.parent.is_none(), "failed adoption consumed owner")?;
    let collision = adopt(other.table(), first, rejected.child)?;
    require(back.dispose() == Err(Error::AccessDenied), "borrowed ref disposed parent's child")?;
    require(parent.reparent(back, other.table(), first) == Err(Error::AlreadyExists),
            "reparent ignored collision")?;
    require(back.info()?.parent == Some(parent.control().id()),
            "failed reparent lost original parent")?;
    // Same edge/cell is a true no-op, preserving an active membership cursor.
    let mut stable = parent.children();
    let _ = stable.next().ok_or(Failure::Mismatch("missing child before no-op"))??;
    parent.reparent(front, parent, second)?;
    require(stable.next().ok_or(Failure::Mismatch("lost child after no-op"))?.is_ok(),
            "no-op invalidated iterator")?;
    let mut changed = parent.children();
    let _ = changed.next().ok_or(Failure::Mismatch("missing child before move"))??;
    parent.reparent(front,
                    parent,
                    Cell { row:    1,
                           column: 0 })?;
    require(matches!(changed.next(), Some(Err(Error::ServiceBusy))),
            "membership cursor accepted changed tree")?;
    parent.reparent(front, parent, second)?;
    system.log("REFLOW_TREE ownership=adopt,collision-return,reparent-rollback \
                cursor=noop,stale\n")?;

    let aggregate = table.control().ask_size(Default::default())?;
    require(aggregate.intrinsic.preferred().width().pixels() == 200,
            "populated Table omitted child creation-width preferences")?;
    let staged = button(system, window.id(), "Paint transaction")?;
    system.windows().begin_update(window.id())?;
    let refused = parent.set_child(Cell { row:    3,
                                          column: 0 },
                                   staged)
                        .err()
                        .ok_or(Failure::Mismatch("adoption crossed pending paint publication"))?;
    require(refused.cause == Error::ServiceBusy
            && refused.child.control().info()?.parent.is_none(),
            "paint refusal lost ownership")?;
    system.windows().end_update(window.id())?;
    let staged = adopt(parent,
                       Cell { row:    3,
                              column: 0 },
                       refused.child)?;
    drop(parent.remove_child(staged)?);

    // Submit snapshots in the opposite order; resident tree order must prevail.
    table.control().draw()?;
    front.draw()?;
    back.draw()?;
    let point = Point { h: 32,
                        v: 28 };
    require(parent.hit_test(point)?
                  .is_some_and(|h| {
                      h.control.id() == front.id() && h.part == controls::Part::Button
                  }),
            "hit query disagrees with tree order")?;
    system.log("REFLOW_TREE_PAINT_READY order=front-cell\n")?;
    system.client().wait_event(0, 120)?;
    // The event boundary now consumes pending Table placement. Capture/clip
    // checks use the published child rectangle rather than its creation point.
    let placed = front.info()?.standard.bounds;
    let front_point = Point { h: placed.left + 16,
                              v: placed.top + 12 };
    let outside_clip = Point { h: placed.left + 60,
                               v: placed.top + 12 };
    require(front.begin_tracking(front_point, 100)?.tracking, "child did not capture")?;
    require(matches!(parent.remove_child(front), Err(Error::ServiceBusy)), "live capture moved")?;
    require(matches!(table.control()
                          .configure(StandardKind::Table,
                                     StandardState::new(Rect::new(8, 8, 200, 160), "")),
                     Err(Error::ServiceBusy)),
            "parent resize bypassed child capture")?;
    require(front.info()?.tracking, "refused transfer lost capture")?;
    front.cancel()?;
    let removed = parent.remove_child(front)?;
    require(removed.control().info()?.parent.is_none(), "removal failed to return root owner")?;
    let front = adopt(parent, second, removed)?;
    table.control().set_visible_rect(Rect::new(8, 8, placed.left + 40, 48).try_into().unwrap())?;
    require(parent.hit_test(outside_clip)?.is_none(), "tree hit escaped ancestor clip")?;
    require(!front.begin_tracking(outside_clip, 200)?.handled,
            "direct capture escaped ancestor clip")?;
    require(front.begin_tracking(front_point, 210)?.tracking, "visible child capture failed")?;
    let released = front.poll_tracking(outside_clip, false, 211)?;
    require(!released.action && !released.tracking, "outside-ancestor release activated child")?;
    table.control().set_visible_rect(Rect::new(8, 8, 240, 160).try_into().unwrap())?;
    system.log("REFLOW_TREE capture=refuse,cancel,retry clip=ancestor release=cancelled\n")?;

    let nested =
        system.controls().create_table(window.id(), Rect::new(8, 8, 220, 150), Reflow::Yes)?;
    let nested_ref = nested.table();
    let deep = adopt(nested_ref, first, button(system, window.id(), "Nested")?)?;
    let nested_control = adopt(parent,
                               Cell { row:    2,
                                      column: 0 },
                               nested.into_control())?;
    require(parent.reparent(nested_control, nested_ref, second) == Err(Error::BadArgument),
            "cycle accepted")?;
    // Self/ancestor adoption failure must return the original owning root.
    let refused = nested_ref.set_child(second, table.into_control())
                            .err()
                            .ok_or(Failure::Mismatch("ancestor adoption accepted"))?;
    require(refused.cause == Error::BadArgument, "wrong cycle cause")?;
    let root_owner = refused.child;
    let remote_window = system.windows()
                              .create_owned(windows::WindowSpec::new(Rect::new(400, 70, 700, 280),
                                                                     "Different window"))?;
    let remote = system.controls()
                       .create_table(remote_window.id(), Rect::new(8, 8, 220, 150), Reflow::Yes)?;
    require(parent.reparent(back, remote.table(), first) == Err(Error::NotSupported),
            "cross-window transfer accepted")?;
    require(back.info()?.parent == Some(parent.control().id()),
            "cross-window refusal changed owner")?;
    let unopted = system.controls().create_standard(window.id(),
                                                     StandardKind::Button,
                                                     StandardState::new(Rect::new(16, 16, 116,
                                                                                  44),
                                                                        "No reflow"),
                                                     Reflow::No)?;
    let refused = parent.set_child(Cell { row:    3,
                                          column: 0 },
                                   unopted)
                        .err()
                        .ok_or(Failure::Mismatch("unopted child accepted"))?;
    require(refused.cause == Error::NotSupported
            && refused.child.control().info()?.parent.is_none(),
            "unopted owner lost")?;
    drop(refused);

    // A successful transfer makes the old parent disposable independently.
    parent.reparent(front, other.table(), second)?;
    window.select()?;
    require(deep.begin_tracking(point, 300)?.tracking,
            "nested capture did not start before subtree disposal")?;
    drop(root_owner);
    for stale in [parent.control(), back, nested_control, deep]
    {
        require(matches!(stale.info(), Err(Error::NotFound)),
                "subtree ref survived parent disposal")?;
    }
    require(front.info()?.parent == Some(other.control().id()),
            "reparented child died with old parent")?;
    let restored = other.table().remove_child(front)?;
    drop(other);
    require(matches!(collision.info(), Err(Error::NotFound)), "other subtree not reclaimed")?;
    require(restored.control().info()?.parent.is_none(), "removed child died with old parent")?;
    drop(restored);
    let fresh = button(system, window.id(), "Reused")?;
    require(fresh.id() != back.id() && matches!(back.info(), Err(Error::NotFound)),
            "generation history reused stale child")?;
    require(fresh.control().begin_tracking(point, 310)?.tracking,
            "disposed subtree stranded an active capture")?;
    fresh.control().cancel()?;
    drop(fresh);
    drop(remote);
    drop(remote_window);
    drop(window);
    let after = pool(system)?;
    require((after.live, after.pending) == (before.live, before.pending),
            "subtree payloads or cleanup tickets leaked")?;
    require(after.births - before.births == after.deaths - before.deaths,
            "tree control births were not balanced by actual disposal")?;
    system.log("REFLOW_TREE_NATIVE_DONE cycle=refused cross-window=refused subtree=stale \
                removed=survives reuse=fresh ledger=balanced\n")?;
    Ok(())
}
