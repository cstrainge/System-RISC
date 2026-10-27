//! Public managed-control calls under a thread-local allocation observer/refuser.
//! The System allocator is unchanged; unrelated test threads are never affected.
#![cfg(not(target_arch = "riscv64"))]
use std::{alloc::{GlobalAlloc, Layout, System as HostAllocator},
          cell::Cell};

use mac_system::testing::with_services;
use mac_toolbox::{Error, Point, Rect, System, WindowId,
                  controls::{StandardKind, StandardState}};
thread_local! {
    static MODE:Cell<u8> = const { Cell::new(0) };
    static ALLOCATIONS:Cell<usize> = const { Cell::new(0) };
}
struct Allocator;
fn allocation() -> bool
{
    MODE.try_with(|mode| {
            let value = mode.get();
            if value != 0
            {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
            value == 2
        })
        .unwrap_or(false)
}
// SAFETY: delegates layouts/pointers unchanged to the host allocator. Only the
// current test thread can select typed allocation refusal, before its API call.
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if allocation() { core::ptr::null_mut() } else { unsafe { HostAllocator.alloc(layout) } }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        unsafe { HostAllocator.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        if allocation()
        {
            core::ptr::null_mut()
        }
        else
        {
            unsafe { HostAllocator.realloc(pointer, layout, size) }
        }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
struct Observe;
impl Observe
{
    fn begin(refuse: bool) -> Self
    {
        ALLOCATIONS.with(|n| n.set(0));
        MODE.with(|m| {
                assert_eq!(m.get(), 0);
                m.set(if refuse { 2 } else { 1 });
            });
        Self
    }

    fn finish(self) -> usize
    {
        drop(self);
        ALLOCATIONS.with(Cell::get)
    }
}
impl Drop for Observe
{
    fn drop(&mut self)
    {
        MODE.with(|m| m.set(0));
    }
}
#[test]
fn managed_draw_highlight_value_and_cancel_do_not_allocate()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let state = StandardState::new(Rect::new(4, 4, 84, 26), "Continue");
        let button =
            system.controls().create_standard(WindowId(0), StandardKind::Button, state).unwrap();
        let control = button.control();
        control.draw().unwrap();
        let observed = Observe::begin(false);
        let mut result = Ok(());
        for _ in 0..32
        {
            result = result.and_then(|()| control.draw());
        }
        let tracked = control.begin_tracking(Point { h: 20,
                                                     v: 12 },
                                             0);
        let polled = control.poll_tracking(Point { h: 21,
                                                   v: 12 },
                                           true,
                                           1);
        let cancelled = control.cancel();
        let mut changed = state;
        changed.value = 1;
        let configured = control.configure(StandardKind::Button, changed);
        let allocations = observed.finish();
        assert!(result.is_ok()
                && tracked.is_ok()
                && polled.is_ok()
                && cancelled.is_ok()
                && configured.is_ok());
        assert_eq!(allocations, 0,
                   "unchanged title/appearance-only snapshots must share admitted backing");
        assert!(!control.info().unwrap().tracking);
    });
}
#[test]
fn title_allocation_refusal_preserves_state_capture_and_retry()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let state = StandardState::new(Rect::new(4, 4, 84, 26), "Before");
        let button =
            system.controls().create_standard(WindowId(0), StandardKind::Button, state).unwrap();
        let control = button.control();
        control.draw().unwrap();
        assert!(control.begin_tracking(Point { h: 20,
                                               v: 12 },
                                       0)
                       .unwrap()
                       .tracking);
        let before = control.info().unwrap();
        let mut next = StandardState::new(Rect::new(4, 4, 94, 26), "After");
        next.value = 1;
        let observed = Observe::begin(true);
        let result = control.configure(StandardKind::Button, next);
        let allocations = observed.finish();
        assert_eq!(result.err(), Some(Error::NoMemory));
        assert!(result.err().unwrap().to_string().contains("memory"));
        assert!(allocations > 0, "test must actually refuse the new title backing");
        let after = control.info().unwrap();
        assert!(before.standard == after.standard && before.tracking == after.tracking,
                "refused configuration changed published state or capture");
        let observed = Observe::begin(true);
        let cancel = control.cancel();
        let draw = control.draw();
        let allocations = observed.finish();
        assert!(cancel.is_ok() && draw.is_ok(),
                "cancellation and existing-title draw must survive heap exhaustion");
        assert_eq!(allocations, 0);
        assert!(!control.info().unwrap().tracking);
        assert!(control.configure(StandardKind::Button, next).is_ok(),
                "a refused operation must remain retryable");
        assert!(control.info().unwrap().standard == next);
    });
}
