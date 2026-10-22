//! Public SDK conformance over the resident drag store and original A-line gate.
#![cfg(not(target_arch = "riscv64"))]
use std::{future::Future,
          pin::pin,
          sync::atomic::{AtomicUsize, Ordering},
          task::{Context, Poll, Waker}};

use mac_system::testing::{self, drag as input};
use mac_toolbox::{drag::*, *};
const WINDOW: WindowId = WindowId(2);
const ANCHOR: Point = Point { h: 10,
                              v: 40 };
const DROP: Point = Point { h: 40,
                            v: 70 };
#[test]
fn packaged_resource_abi_matches_the_independent_runtime_abi()
{
    assert_eq!(mac_code_resource::ABI_VERSION, mac_toolbox::abi::ABI_VERSION);
}
fn poll<F: Future>(future: core::pin::Pin<&mut F>) -> Poll<F::Output>
{
    future.poll(&mut Context::from_waker(Waker::noop()))
}
fn entered<'a>(system: &System<'a>) -> TrackingOffer<'a>
{
    input::motion(DROP, 0, WINDOW, 1);
    match system.next_event(EventMask::ALL).unwrap()
    {
        Event::Drag { offer: DragEvent::Tracking(offer),
                      .. } => offer,
        event => panic!("{event:?}")
    }
}
fn dropped<'a>(system: &System<'a>) -> DropPayload<'a>
{
    input::release(DROP, 0, WINDOW, 2);
    match system.next_event(EventMask::ALL).unwrap()
    {
        Event::Drag { offer: DragEvent::Dropped(drop),
                      .. } => drop,
        event => panic!("{event:?}")
    }
}
fn ended(system: &System<'_>) -> DragOutcome
{
    for _ in 0..10
    {
        match system.next_event(EventMask::ALL).unwrap()
        {
            Event::Drag { offer: DragEvent::Ended { outcome },
                          .. } => return outcome,
            Event::Drag { offer: DragEvent::Dropped(_),
                          .. } => panic!("unexpected drop before terminal outcome"),
            _ => ()
        }
    }
    panic!("missing terminal drag event")
}
#[test]
fn inline_data_is_drop_only_take_once_and_move_is_an_outcome()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::new(0, 30, 20, 50)).unwrap();
        builder.flavor(Flavor::text("Hello é")).unwrap();
        let session = builder.begin().unwrap();
        let offer = entered(&system);
        let item = offer.flavors().item(0).unwrap();
        assert!(offer.flavors().has::<Text<'_>>(item).unwrap());
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::new(20, 40, 60, 90) })
             .unwrap();
        let drop = dropped(&system);
        assert_eq!(drop.action, DropAction::Move);
        {
            let mut bytes = [0; 64];
            let mut take = pin!(drop.text(item, &mut bytes));
            assert_eq!(poll(take.as_mut()), Poll::Ready(Ok("Hello é")));
            let mut second = [0; 64];
            let mut take = pin!(drop.text(item, &mut second));
            assert_eq!(poll(take.as_mut()),
                       Poll::Ready(Err(DragError::FlavorUnavailable { flavor: TEXT })));
        }
        let mut complete = pin!(drop.receive(async { Ok(()) }));
        assert_eq!(poll(complete.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(ended(&system), DragOutcome::Moved);
        drop_owner(session);
        assert!(system.next_event(EventMask::ALL).unwrap().is_idle());
    });
}
fn drop_owner<T>(owner: T)
{
    drop(owner);
}
#[test]
fn cancellation_wakes_pending_take_and_duplicate_take_cannot_steal_its_waker()
{
    use std::sync::Arc;
    struct Counter(AtomicUsize);
    impl std::task::Wake for Counter
    {
        fn wake(self: Arc<Self>)
        {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        let source = builder.promise::<Text<'_>, _>(|| panic!("cancelled producer must not run"))
                            .unwrap()
                            .begin()
                            .unwrap();
        let offer = entered(&system);
        let item = offer.flavors().item(0).unwrap();
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::ZERO })
             .unwrap();
        let payload = dropped(&system);
        let count = Arc::new(Counter(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let mut bytes = [0; 32];
        let mut take = pin!(payload.text(item, &mut bytes));
        assert!(take.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
        {
            let mut other = [0; 32];
            let mut duplicate = pin!(payload.text(item, &mut other));
            assert_eq!(poll(duplicate.as_mut()),
                       Poll::Ready(Err(DragError::FlavorUnavailable { flavor: TEXT })));
        }
        source.cancel().unwrap();
        for _ in 0..4
        {
            let _ = system.next_event(EventMask::ALL).unwrap();
        }
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll(take.as_mut()), Poll::Ready(Err(DragError::NoSession)));
        for tick in 1..9
        {
            input::tick(tick);
        }
        assert_eq!(ended(&system), DragOutcome::Cancelled);
    });
}

#[test]
fn classic_text_sizes_and_offsets_use_macroman_while_native_takes_utf8()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        builder.flavor(Flavor::text("Café / ﬁ")).unwrap();
        let _source = builder.begin().unwrap();
        let offer = entered(&system);
        // Original callbacks carry this same reference; transport extraction is
        // confined to the classic A-line conformance fixture.
        let event = DragEvent::Tracking(offer);
        let id = event.as_record(0, 0).payload as u32;
        let DragEvent::Tracking(offer) = event
        else
        {
            unreachable!()
        };
        let item = offer.flavors().item(0).unwrap();
        let mut memory = [0u8; 512];
        put(&mut memory, 0, 100);
        memory[4..8].copy_from_slice(b"TEXT");
        put(&mut memory, 8, 1);
        put(&mut memory, 12, id);
        assert_eq!(classic(&system, 0x13, &mut memory, 16), 0);
        assert_eq!(long(&memory, 100), 8);
        put(&mut memory, 0, 3);
        put(&mut memory, 4, 1);
        put(&mut memory, 8, 200);
        memory[12..16].copy_from_slice(b"TEXT");
        put(&mut memory, 16, 1);
        put(&mut memory, 20, id);
        memory[200] = 0x8f;
        assert_eq!(classic(&system, 9, &mut memory, 24), 0);
        put(&mut memory, 0, 3);
        put(&mut memory, 4, 100);
        put(&mut memory, 8, 220);
        put(&mut memory, 100, 5);
        assert_eq!(classic(&system, 0x14, &mut memory, 24), 0);
        assert_eq!(&memory[220..225], b"\x8f / \xde");
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::ZERO })
             .unwrap();
        let payload = dropped(&system);
        {
            let mut bytes = [0; 32];
            let mut take = pin!(payload.text(item, &mut bytes));
            assert_eq!(poll(take.as_mut()), Poll::Ready(Ok("Cafè / ﬁ")));
        }
        let mut complete = pin!(payload.receive(async { Ok(()) }));
        assert_eq!(poll(complete.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(ended(&system), DragOutcome::Moved);
    });
}

#[test]
fn every_classic_text_byte_is_decoded_for_a_native_receiver()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut memory = [0u8; 1024];
        put(&mut memory, 0, 100);
        assert_eq!(classic(&system, 5, &mut memory, 4), 0);
        let id = long(&memory, 100);
        let bytes: Vec<u8> = (0..=255).collect();
        memory[200..456].copy_from_slice(&bytes);
        put(&mut memory, 0, 0);
        put(&mut memory, 4, 256);
        put(&mut memory, 8, 200);
        memory[12..16].copy_from_slice(b"TEXT");
        put(&mut memory, 16, 0x77);
        put(&mut memory, 20, id);
        assert_eq!(classic(&system, 7, &mut memory, 24), 0);
        // The callback execution adapter will call this same resident begin.
        // This fixture tests data projection without claiming 68k execution.
        let mut begin = abi::drag::Call { operation: abi::drag::BEGIN,
                                          id,
                                          point: ANCHOR,
                                          bounds: Rect::new(0, 30, 20, 50),
                                          ..Default::default() };
        system.client().drag(&mut begin, &[], &mut []).unwrap();
        let offer = entered(&system);
        let item = offer.flavors().item(0).unwrap();
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::ZERO })
             .unwrap();
        let payload = dropped(&system);
        {
            let mut out = [0; 1024];
            let mut take = pin!(payload.text(item, &mut out));
            let expected = mac_formats::text::decode(&bytes);
            assert_eq!(poll(take.as_mut()), Poll::Ready(Ok(expected.as_str())));
        }
        let mut complete = pin!(payload.receive(async { Ok(()) }));
        assert_eq!(poll(complete.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(ended(&system), DragOutcome::Moved);
        put(&mut memory, 0, id);
        assert_eq!(classic(&system, 6, &mut memory, 4), 0);
    });
}

#[test]
fn modifiers_are_sampled_at_release_and_trash_always_moves()
{
    use abi::events::{CMD_KEY, OPTION_KEY};
    for (before, after, trash, expected) in [(OPTION_KEY, CMD_KEY, false, DropAction::Move),
                                             (CMD_KEY, OPTION_KEY, false, DropAction::Copy),
                                             (0, CMD_KEY | OPTION_KEY, false, DropAction::Move),
                                             (0, OPTION_KEY, true, DropAction::Move)]
    {
        testing::with_services(|services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            input::receiver(1, 1, WINDOW);
            let mut builder =
                system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            builder.empty_item(Rect::ZERO).unwrap();
            builder.flavor(Flavor::text("drop")).unwrap();
            let _source = builder.begin().unwrap();
            input::motion(DROP, before, WINDOW, 1);
            let Event::Drag { offer: DragEvent::Tracking(offer),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing enter")
            };
            let target = if trash
            {
                Target::Trash { volume: VolumeId::NONE,
                                id:     files::ROOT_ID }
            }
            else
            {
                Target::Content
            };
            offer.respond(Response::Accept { actions: DropActions::CopyOrMove,
                                             target,
                                             highlight: Rect::ZERO })
                 .unwrap();
            input::release(DROP, after, WINDOW, 2);
            let Event::Drag { offer: DragEvent::Tracking(offer),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing modifier update")
            };
            // A late physical report after release cannot change the decision.
            input::motion(Point { h: 80,
                                  v: 90 },
                          before,
                          WINDOW,
                          3);
            offer.respond(Response::Accept { actions: DropActions::CopyOrMove,
                                             target,
                                             highlight: Rect::ZERO })
                 .unwrap();
            let Event::Drag { offer: DragEvent::Dropped(payload),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing drop")
            };
            assert_eq!(payload.action, expected);
            assert_eq!(payload.position(), DROP);
            let mut done = pin!(payload.receive(async { Ok(()) }));
            assert_eq!(poll(done.as_mut()), Poll::Ready(Ok(())));
            assert_eq!(ended(&system),
                       if expected == DropAction::Move
                       {
                           DragOutcome::Moved
                       }
                       else
                       {
                           DragOutcome::Copied
                       });
        });
    }
}

#[test]
fn invalid_native_text_is_rejected_and_unrepresentable_classic_text_is_not_utf8()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        assert_eq!(builder.flavor(Flavor::custom(TEXT, &[0xff])),
                   Err(DragError::Unknown(abi::BAD_ARGUMENT)));
        builder.flavor(Flavor::text("🦀")).unwrap();
        let _source = builder.begin().unwrap();
        let offer = entered(&system);
        let event = DragEvent::Tracking(offer);
        let id = event.as_record(0, 0).payload as u32;
        let DragEvent::Tracking(_offer) = event
        else
        {
            unreachable!()
        };
        let mut memory = [0xa5u8; 512];
        put(&mut memory, 0, 0);
        put(&mut memory, 4, 100);
        put(&mut memory, 8, 220);
        memory[12..16].copy_from_slice(b"TEXT");
        put(&mut memory, 16, 1);
        put(&mut memory, 20, id);
        put(&mut memory, 100, 16);
        assert_eq!(classic(&system, 0x14, &mut memory, 24), -1854);
        assert_eq!(&memory[220..236], &[0xa5; 16]);
        assert_eq!(long(&memory, 100), 16);
    });
}
#[test]
fn same_app_promise_runs_once_only_after_drop_requests_it()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let count = AtomicUsize::new(0);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::new(0, 30, 20, 50)).unwrap();
        let builder = builder.promise::<Text<'_>, _>(|| {
                                 count.fetch_add(1, Ordering::SeqCst);
                                 Ok(Text::new("lazy"))
                             })
                             .unwrap();
        let mut session = builder.begin().unwrap();
        let offer = entered(&system);
        let item = offer.flavors().item(0).unwrap();
        assert!(offer.flavors().has::<Text<'_>>(item).unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(!session.service_promises().unwrap());
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::default() })
             .unwrap();
        let drop = dropped(&system);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        {
            let mut output = [0; 16];
            let mut take = pin!(drop.text(item, &mut output));
            assert!(poll(take.as_mut()).is_pending());
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert!(session.service_promises().unwrap());
            assert_eq!(poll(take.as_mut()), Poll::Ready(Ok("lazy")));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(!session.service_promises().unwrap());
        }
        let mut complete = pin!(drop.receive(async { Ok(()) }));
        assert_eq!(poll(complete.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(ended(&system), DragOutcome::Moved);
    });
}

#[test]
fn a_rejected_promise_payload_completes_with_a_typed_error()
{
    struct InvalidText;
    impl FlavorData for InvalidText
    {
        const TYPE: FlavorType = TEXT;

        fn bytes(&self) -> &[u8]
        {
            &[0xff]
        }
    }
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        let mut source =
            builder.promise::<InvalidText, _>(|| Ok(InvalidText)).unwrap().begin().unwrap();
        let offer = entered(&system);
        let item = offer.flavors().item(0).unwrap();
        offer.respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                         target:    Target::Content,
                                         highlight: Rect::ZERO })
             .unwrap();
        let payload = dropped(&system);
        {
            let mut bytes = [0; 32];
            let mut take = pin!(payload.text(item, &mut bytes));
            assert!(poll(take.as_mut()).is_pending());
            assert!(source.service_promises().unwrap());
            assert_eq!(poll(take.as_mut()),
                       Poll::Ready(Err(DragError::PromiseFailed { flavor: TEXT,
                                                                  source: Error::BadArgument })));
            assert!(!source.service_promises().unwrap());
        }
    });
}
#[test]
fn owner_drop_zooms_back_and_delivers_exactly_one_cancelled_outcome()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::new(0, 30, 20, 50)).unwrap();
        builder.flavor(Flavor::text("cancel me")).unwrap();
        let owner = builder.begin().unwrap();
        let _ = entered(&system);
        assert!(input::feedback().is_some());
        drop(owner);
        assert!(input::feedback().is_some());
        for tick in 2..9
        {
            input::tick(tick);
        }
        assert_eq!(input::feedback(), None);
        assert_eq!(ended(&system), DragOutcome::Cancelled);
        assert!(system.next_event(EventMask::ALL).unwrap().is_idle());
    });
}
#[test]
fn source_is_send_sync_but_operations_validate_the_os_thread()
{
    fn mobile<T: Send + Sync>()
    {
    }
    mobile::<DragSession<'static>>();
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut b = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        b.empty_item(Rect::default()).unwrap();
        let session = b.begin().unwrap();
        assert_eq!(input::with_thread(99, || session.cancel()), Err(DragError::WrongThread));
        assert_eq!(testing::files::with_owner(99, || session.cancel()),
                   Err(DragError::Unknown(abi::ACCESS_DENIED)));
        session.cancel().unwrap();
        assert_eq!(ended(&system), DragOutcome::Cancelled);
    });
}
fn classic(system: &System<'_>, selector: u32, memory: &mut [u8], args: u32) -> i16
{
    let mut registers = abi::traps::ClassicRegisters { pc: 0x4000,
                                                       ..Default::default() };
    registers.d[0] = selector;
    system.client().classic(abi::traps::classic::DRAG_DISPATCH, &mut registers, 0, memory).unwrap();
    assert_eq!(registers.a[7], args);
    assert_eq!(registers.pc, 0x4002);
    i16::from_be_bytes(memory[args as usize..args as usize + 2].try_into().unwrap())
}
fn put(memory: &mut [u8], at: usize, value: u32)
{
    memory[at..at + 4].copy_from_slice(&value.to_be_bytes());
}
fn long(memory: &[u8], at: usize) -> u32
{
    u32::from_be_bytes(memory[at..at + 4].try_into().unwrap())
}
#[test]
fn original_pascal_items_offsets_repeat_reads_and_stale_drag_references()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut memory = [0u8; 512];
        put(&mut memory, 0, 100);
        assert_eq!(classic(&system, 5, &mut memory, 4), 0);
        let id = long(&memory, 100);
        // AddDragItemFlavor auto-creates an item; its reference is not its index.
        put(&mut memory, 0, 4);
        put(&mut memory, 4, 6);
        put(&mut memory, 8, 200);
        memory[12..16].copy_from_slice(b"TEXT");
        put(&mut memory, 16, 0x76543210);
        put(&mut memory, 20, id);
        memory[200..206].copy_from_slice(b"abcdef");
        assert_eq!(classic(&system, 7, &mut memory, 24), 0);
        put(&mut memory, 0, 120);
        put(&mut memory, 4, id);
        assert_eq!(classic(&system, 0xe, &mut memory, 8), 0);
        assert_eq!(&memory[120..122], &[0, 1]);
        put(&mut memory, 0, 124);
        memory[4..6].copy_from_slice(&1u16.to_be_bytes());
        put(&mut memory, 6, id);
        assert_eq!(classic(&system, 0xf, &mut memory, 10), 0);
        assert_eq!(long(&memory, 124), 0x76543210);
        memory[4..6].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(classic(&system, 0xf, &mut memory, 10), -1851);
        // A classic partial read remains repeatable, unlike native take-once.
        for _ in 0..2
        {
            put(&mut memory, 0, 2);
            put(&mut memory, 4, 128);
            put(&mut memory, 8, 220);
            memory[12..16].copy_from_slice(b"TEXT");
            put(&mut memory, 16, 0x76543210);
            put(&mut memory, 20, id);
            put(&mut memory, 128, 3);
            assert_eq!(classic(&system, 0x14, &mut memory, 24), 0);
            assert_eq!(long(&memory, 128), 3);
            assert_eq!(&memory[220..223], b"cde");
        }
        put(&mut memory, 0, id);
        assert_eq!(classic(&system, 6, &mut memory, 4), 0);
        put(&mut memory, 0, 100);
        assert_eq!(classic(&system, 5, &mut memory, 4), 0);
        let replacement = long(&memory, 100);
        assert_eq!(id & 0xffff, replacement & 0xffff);
        assert_ne!(id, replacement);
        put(&mut memory, 0, 120);
        put(&mut memory, 4, id);
        assert_eq!(classic(&system, 0xe, &mut memory, 8), -1850);
        put(&mut memory, 0, replacement);
        assert_eq!(classic(&system, 6, &mut memory, 4), 0);
    });
}

#[test]
fn ignored_tracking_after_release_rejects_and_peek_never_owns_the_protocol()
{
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        let source = builder.begin().unwrap();
        input::motion(DROP, 0, WINDOW, 1);
        input::release(DROP, 0, WINDOW, 2);
        let before = input::feedback();
        for _ in 0..3
        {
            let snapshot = system.events().peek(EventMask::ALL).unwrap().unwrap();
            assert_eq!(snapshot.into_record().message & 0xff00_0000, abi::drag::ENTERED_EVENT);
            assert_eq!(input::feedback(), before);
        }
        // No handler, response call, or receive future is necessary for cleanup.
        drop(system.next_event(EventMask::ALL).unwrap());
        for tick in 1..9
        {
            input::tick(tick);
        }
        assert_eq!(ended(&system), DragOutcome::Rejected);
        assert!(system.next_event(EventMask::ALL).unwrap().is_idle());
        drop(source);
        let mut next = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        next.empty_item(Rect::ZERO).unwrap();
        let next = next.begin().unwrap();
        next.cancel().unwrap();
        assert_eq!(ended(&system), DragOutcome::Cancelled);
    });
}

#[test]
fn owned_drops_and_receive_futures_reject_on_every_abandonment_path()
{
    for scenario in 0..5
    {
        testing::with_services(|services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            input::receiver(1, 1, WINDOW);
            let mut builder =
                system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            builder.empty_item(Rect::ZERO).unwrap();
            let source = builder.begin().unwrap();
            entered(&system).respond(Response::Accept { actions:   DropActions::CopyOrMove,
                                                        target:    Target::Content,
                                                        highlight: Rect::ZERO })
                            .unwrap();
            let payload = dropped(&system);
            match scenario
            {
                0 => drop(payload),
                1 => drop(payload.receive(async { Ok(()) })), // never polled
                2 =>
                {
                    let mut future = pin!(payload.receive(core::future::pending()));
                    assert!(poll(future.as_mut()).is_pending());
                }
                3 =>
                {
                    let mut future = pin!(payload.receive(async {
                                                     Err(DragError::Unknown(abi::NOT_SUPPORTED))
                                                 }));
                    assert_eq!(poll(future.as_mut()),
                               Poll::Ready(Err(DragError::Unknown(abi::NOT_SUPPORTED))));
                }
                4 => input::with_thread(99, || drop(payload)),
                _ => unreachable!()
            }
            for tick in 1..9
            {
                input::tick(tick);
            }
            assert_eq!(ended(&system), DragOutcome::Rejected, "scenario {scenario}");
            assert!(system.next_event(EventMask::ALL).unwrap().is_idle());
            drop(source);
            let mut next = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            next.empty_item(Rect::ZERO).unwrap();
            let next = next.begin().unwrap();
            next.cancel().unwrap();
            assert_eq!(ended(&system), DragOutcome::Cancelled);
        });
    }
}

#[test]
fn receivers_declare_actions_and_never_coerce_a_forced_action()
{
    use abi::events::OPTION_KEY;
    for (actions, modifiers, expected) in
        [(DropActions::Move, OPTION_KEY, None),
         (DropActions::Copy, 0, None),
         (DropActions::Copy, OPTION_KEY, Some(DropAction::Copy)),
         (DropActions::Move, 0, Some(DropAction::Move)),
         (DropActions::CopyOrMove, OPTION_KEY, Some(DropAction::Copy))]
    {
        testing::with_services(|services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            input::receiver(1, 1, WINDOW);
            let mut builder =
                system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            builder.empty_item(Rect::ZERO).unwrap();
            builder.flavor(Flavor::text("data")).unwrap();
            let _source = builder.begin().unwrap();
            input::motion(DROP, modifiers, WINDOW, 1);
            let Event::Drag { offer: DragEvent::Tracking(offer),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing enter")
            };
            offer.respond(Response::Accept { target: Target::Content,
                                             actions,
                                             highlight: Rect::new(20, 40, 60, 90) })
                 .unwrap();
            assert_eq!(input::highlight().is_some(), expected.is_some());
            input::release(DROP, modifiers, WINDOW, 2);
            if let Some(expected) = expected
            {
                let Event::Drag { offer: DragEvent::Dropped(payload),
                                  .. } = system.next_event(EventMask::ALL).unwrap()
                else
                {
                    panic!("missing supported drop")
                };
                assert_eq!(payload.action, expected);
                let mut done = pin!(payload.receive(async { Ok(()) }));
                assert_eq!(poll(done.as_mut()), Poll::Ready(Ok(())));
                assert_eq!(ended(&system),
                           if expected == DropAction::Copy
                           {
                               DragOutcome::Copied
                           }
                           else
                           {
                               DragOutcome::Moved
                           });
            }
            else
            {
                for tick in 3..12
                {
                    input::tick(tick);
                }
                assert_eq!(ended(&system), DragOutcome::Rejected);
            }
            assert!(input::highlight().is_none());
        });
    }
}

#[test]
fn modifier_change_withdraws_highlight_before_receiver_runs_and_release_can_restore_it()
{
    use abi::events::{CMD_KEY, OPTION_KEY};
    testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        input::receiver(1, 1, WINDOW);
        let mut builder = system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
        builder.empty_item(Rect::ZERO).unwrap();
        let _source = builder.begin().unwrap();
        entered(&system).respond(Response::Accept { target:    Target::Content,
                                                    actions:   DropActions::Move,
                                                    highlight: Rect::new(20, 40, 60, 90) })
                        .unwrap();
        assert!(input::highlight().is_some());
        input::motion(DROP, OPTION_KEY, WINDOW, 2);
        assert!(input::highlight().is_none(),
                "do not wait for an app to remove impossible feedback");
        let Event::Drag { offer: DragEvent::Tracking(offer),
                          .. } = system.next_event(EventMask::ALL).unwrap()
        else
        {
            panic!("missing modifier update")
        };
        offer.respond(Response::Accept { target:    Target::Content,
                                         actions:   DropActions::Move,
                                         highlight: Rect::new(20, 40, 60, 90) })
             .unwrap();
        assert!(input::highlight().is_none());
        input::release(DROP, CMD_KEY, WINDOW, 3);
        let Event::Drag { offer: DragEvent::Tracking(offer),
                          .. } = system.next_event(EventMask::ALL).unwrap()
        else
        {
            panic!("missing release update")
        };
        offer.respond(Response::Accept { target:    Target::Content,
                                         actions:   DropActions::Move,
                                         highlight: Rect::new(20, 40, 60, 90) })
             .unwrap();
        let Event::Drag { offer: DragEvent::Dropped(payload),
                          .. } = system.next_event(EventMask::ALL).unwrap()
        else
        {
            panic!("missing supported release")
        };
        assert_eq!(payload.action, DropAction::Move);
        let mut done = pin!(payload.receive(async { Ok(()) }));
        assert_eq!(poll(done.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(ended(&system), DragOutcome::Moved);
    });
}

#[test]
fn mounted_policy_rejects_unimplemented_moves_and_all_readonly_destination_actions()
{
    for (read_only, actions, modifiers, accepted) in
        [(false, DropActions::Move, 0, false),
         (true, DropActions::Move, 0, false),
         (true, DropActions::Copy, abi::events::OPTION_KEY, false),
         // Copy can be implemented by the receiver through ordinary read/write
         // services. This tests admission only, not a completed file transfer.
         (false, DropActions::Copy, abi::events::OPTION_KEY, true)]
    {
        testing::with_services(|services| {
            let volume = input::provider_volume(read_only);
            let system = unsafe { System::from_raw(services) }.unwrap();
            input::receiver(1, 1, WINDOW);
            let mut builder =
                system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            builder.empty_item(Rect::ZERO).unwrap();
            let source = builder.begin().unwrap();
            input::motion(DROP, modifiers, WINDOW, 1);
            let Event::Drag { offer: DragEvent::Tracking(offer),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing enter")
            };
            offer.respond(Response::Accept { target: Target::Folder { volume,
                                                                      id: files::ROOT_ID },
                                             actions,
                                             highlight: Rect::new(20, 40, 60, 90) })
                 .unwrap();
            assert_eq!(input::highlight().is_some(), accepted);
            input::release(DROP, modifiers, WINDOW, 2);
            if accepted
            {
                let Event::Drag { offer: DragEvent::Dropped(payload),
                                  .. } = system.next_event(EventMask::ALL).unwrap()
                else
                {
                    panic!("missing supported offer")
                };
                drop(payload);
            }
            for tick in 3..12
            {
                input::tick(tick);
            }
            assert_eq!(ended(&system), DragOutcome::Rejected);
            drop(source);
            assert!(system.next_event(EventMask::ALL).unwrap().is_idle());
        });
    }
}

#[test]
fn readonly_source_copies_by_default_and_cannot_be_forced_to_move_or_trashed()
{
    for (modifiers, trash, accepted) in [(0, false, true),
                                         (abi::events::CMD_KEY, false, false),
                                         (abi::events::OPTION_KEY, true, false)]
    {
        testing::with_services(|services| {
            let volume = input::provider_volume(true);
            let system = unsafe { System::from_raw(services) }.unwrap();
            input::receiver(1, 1, WINDOW);
            let mut builder =
                system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
            builder.item(file_item(volume, Cnid(16), files::ROOT_ID, Point::default(), Rect::ZERO))
                   .unwrap();
            let _source = builder.begin().unwrap();
            input::motion(DROP, modifiers, WINDOW, 1);
            let Event::Drag { offer: DragEvent::Tracking(offer),
                              .. } = system.next_event(EventMask::ALL).unwrap()
            else
            {
                panic!("missing enter")
            };
            let target = if trash
            {
                Target::Trash { volume: VolumeId::NONE,
                                id:     files::ROOT_ID }
            }
            else
            {
                Target::Content
            };
            offer.respond(Response::Accept { target,
                                             actions: DropActions::CopyOrMove,
                                             highlight: Rect::new(20, 40, 60, 90) })
                 .unwrap();
            assert_eq!(input::highlight().is_some(), accepted);
            input::release(DROP, modifiers, WINDOW, 2);
            if accepted
            {
                let Event::Drag { offer: DragEvent::Dropped(payload),
                                  .. } = system.next_event(EventMask::ALL).unwrap()
                else
                {
                    panic!("missing copy")
                };
                assert_eq!(payload.action, DropAction::Copy);
                drop(payload);
            }
            for tick in 3..12
            {
                input::tick(tick);
            }
            assert_eq!(ended(&system), DragOutcome::Rejected);
        });
    }
}

#[test]
fn atomic_folder_move_negotiation_checks_volume_scope_without_disk_io()
{
    struct Disk(std::path::PathBuf);
    impl Drop for Disk
    {
        fn drop(&mut self)
        {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/files/dates-2026.hfs");
    let disk = Disk(std::env::temp_dir().join(format!("drag-target-{}.hfs", std::process::id())));
    std::fs::copy(source, &disk.0).unwrap();
    let (_, baseline) =
        testing::with_file_disk(&disk.0, false, Default::default(), |_| ()).unwrap();
    for (foreign_source, accepted) in [(false, true), (true, false)]
    {
        let (_, activity) =
            testing::with_file_disk(&disk.0, false, Default::default(), |services| {
                let system = unsafe { System::from_raw(services) }.unwrap();
                let target = system.files().volume_at(0).unwrap().id;
                let volume = if foreign_source { input::provider_volume(false) } else { target };
                input::receiver(1, 1, WINDOW);
                let mut builder =
                    system.drags().builder(None, ANCHOR, Rect::new(0, 30, 20, 50)).unwrap();
                builder.item(file_item(volume,
                                       Cnid(16),
                                       files::ROOT_ID,
                                       Point::default(),
                                       Rect::ZERO))
                       .unwrap();
                let _source = builder.begin().unwrap();
                input::motion(DROP, abi::events::CMD_KEY, WINDOW, 1);
                let Event::Drag { offer: DragEvent::Tracking(offer),
                                  .. } = system.next_event(EventMask::ALL).unwrap()
                else
                {
                    panic!("missing enter")
                };
                offer.respond(Response::Accept { target:    Target::Folder { volume: target,
                                                                        id:     files::ROOT_ID },
                                            actions:   DropActions::Move,
                                            highlight: Rect::new(20, 40, 60, 90) })
                .unwrap();
                assert_eq!(input::highlight().is_some(), accepted);
                input::release(DROP, abi::events::CMD_KEY, WINDOW, 2);
                if accepted
                {
                    let Event::Drag { offer: DragEvent::Dropped(payload),
                                      .. } = system.next_event(EventMask::ALL).unwrap()
                    else
                    {
                        panic!("missing admitted move")
                    };
                    // Admission only: do not mutate the fixture.
                    drop(payload);
                }
                for tick in 3..12
                {
                    input::tick(tick);
                }
                assert_eq!(ended(&system), DragOutcome::Rejected);
            }).unwrap();
        assert_eq!(activity.reads, baseline.reads, "negotiation did device I/O");
        assert_eq!(activity.writes, 0);
        assert_eq!(activity.flushes, 0);
    }
}
