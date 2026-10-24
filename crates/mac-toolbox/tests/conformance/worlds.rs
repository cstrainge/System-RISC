//! Full-width offscreen identities and pixels through the public Rust SDK.
use mac_toolbox::{Error, Point, Rect, System,
                  quickdraw::{DrawingTarget, WorldId, graphics::TransferMode}};

use super::Failure;

fn check<T>(case: &'static str, result: Result<T, Error>) -> Result<T, Failure>
{
    result.map_err(|error| {
              Failure::ServiceAt { case,
                                   status: error.status() }
          })
}

pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let q = system.quickdraw();
    let world = check("GWorld create", q.new_gworld(8, 8))?;
    let DrawingTarget::World(stale) = world.graphics().target()
    else
    {
        panic!("GWorld must expose a typed world target");
    };
    assert!(stale.0 > u32::MAX as u64, "native world identity must retain every bit");
    let mut pixels = [0; 64];
    assert_eq!(check("GWorld read", world.pixels(&mut pixels))?, 64);
    assert_eq!(pixels, [0xffffff; 64]);
    check("GWorld colors", world.graphics().colors(0x123456, 0xffffff))?;
    check("GWorld paint", world.graphics().paint_rect(Rect::new(0, 0, 8, 8)))?;
    check("GWorld read", world.pixels(&mut pixels))?;
    assert_eq!(pixels, [0x123456; 64]);
    assert_eq!(world.pixels(&mut [0; 63]), Err(Error::NoMemory));
    assert_eq!(world.dimensions()?,
               Point { h: 8,
                       v: 8 });
    let destination = check("GWorld destination create", q.new_gworld(8, 8))?;
    check("GWorld copy",
          world.copy_to(destination.graphics(),
                        Rect::new(0, 0, 8, 8),
                        Rect::new(0, 0, 8, 8),
                        TransferMode::Copy))?;
    check("GWorld destination read", destination.pixels(&mut pixels))?;
    assert_eq!(pixels, [0x123456; 64]);
    check("GWorld write", world.set_pixels(&[0xabcdef; 64]))?;
    check("GWorld read", world.pixels(&mut pixels))?;
    assert_eq!(pixels, [0xabcdef; 64]);
    drop(world);
    let fresh = check("GWorld recreate", q.new_gworld(8, 8))?;
    assert_ne!(fresh.graphics().target(), DrawingTarget::World(stale));
    assert_eq!(q.graphics(stale).state().err(), Some(Error::NotFound));
    assert_eq!(q.graphics(WorldId(u64::MAX)).state().err(), Some(Error::NotFound));
    assert_eq!(q.graphics(stale).paint_rect(Rect::new(0, 0, 8, 8)), Err(Error::NotFound));
    assert_eq!(fresh.dimensions()?,
               Point { h: 8,
                       v: 8 });
    assert!(matches!(q.new_gworld(0, 8), Err(Error::BadArgument)));
    assert!(matches!(q.new_gworld(641, 480), Err(Error::NoMemory)));
    system.log("GWORLD_USERSPACE_PASS pixels=passed full_width=passed stale=passed \
                recovery=passed\n")?;
    Ok(())
}

/// Native wire validation must reject malformed targets before decoding payloads
/// or drawing. This uses the public escape hatch, never a resident test hook.
pub fn malformed_targets(gate: mac_toolbox::abi::TrapGate)
{
    use mac_toolbox::abi::{self, quickdraw::*, traps::*};
    fn rejected<T>(gate: abi::TrapGate, trap: u32, call: &mut T)
    {
        // Each initialized wire value stays live for this synchronous gate call.
        let status =
            unsafe { gate(trap, core::ptr::from_mut(call).cast(), core::mem::size_of::<T>()) };
        assert_eq!(status, abi::BAD_ARGUMENT as i64, "malformed drawing target on trap {trap:#x}");
    }
    for target in [Target { kind: u32::MAX,
                            ..Default::default() },
                   Target { reserved: 1,
                            ..Default::default() },
                   Target { id: 1,
                            ..Target::desktop() },
                   Target { id: u64::MAX,
                            ..Default::default() }]
    {
        rejected(gate,
                 QUICKDRAW,
                 &mut GraphicsCall { target,
                                     ..Default::default() });
        rejected(gate,
                 FONT,
                 &mut FontCall { target,
                                 ..Default::default() });
        rejected(gate,
                 COPY_BITS,
                 &mut BitmapCall { target,
                                   ..Default::default() });
        rejected(gate,
                 WORLD,
                 &mut WorldCall { target,
                                  ..Default::default() });
        rejected(gate,
                 PICTURE,
                 &mut PictureCall { target,
                                    ..Default::default() });
        rejected(gate,
                 IMAGE,
                 &mut abi::images::ImageCall { target,
                                               ..Default::default() });
        rejected(gate,
                 ICON,
                 &mut IconCall { target,
                                 ..Default::default() });
        rejected(gate,
                 ICON,
                 &mut IconDataCall { target,
                                     ..Default::default() });
        rejected(gate,
                 ICON,
                 &mut FileIconCall { target,
                                     ..Default::default() });
        rejected(gate,
                 ICON,
                 &mut DefaultFileIconCall { target,
                                            ..Default::default() });
    }
}
