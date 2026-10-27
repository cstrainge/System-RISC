//! Startup outcome transport uses real Icon Utilities decoding; expected icon
//! pixels enter through the public SDK. Native VM activation is a separate gate.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_formats::resource::{Resource, encode};
use mac_system::{testing::extension_startup::{Bitmap, Error, Layout, Outcome, Refusal, Source,
                                              Strip},
                 toolbox::quickdraw::icons::IconFamily};
use mac_toolbox::{Point, Rect, ResourceScope,
                  quickdraw::{SurfacePort, icons::IconFlags}};

fn family(id: i16, colored: bool) -> Vec<Resource>
{
    let mut mono = vec![0; 256];
    for y in 2..30
    {
        for x in 2..30
        {
            let offset = y * 4 + x / 8;
            let bit = 0x80 >> (x & 7);
            mono[128 + offset] |= bit;
            if (x + y) % 3 == 0
            {
                mono[offset] |= bit;
            }
        }
    }
    let mut resources = vec![Resource { kind: *b"ICN#",
                                        id,
                                        name: None,
                                        attributes: 0,
                                        data: mono }];
    if colored
    {
        resources.push(Resource { kind: *b"icl8",
                                  id,
                                  name: None,
                                  attributes: 0,
                                  data: (0..1024).map(|at| {
                                                     if (at % 32 + at / 32) & 1 == 0
                                                     {
                                                         35
                                                     }
                                                     else
                                                     {
                                                         180
                                                     }
                                                 })
                                                 .collect() });
    }
    resources
}
fn bitmap(resources: &[Resource], id: i16) -> Bitmap
{
    Bitmap::prepare(&IconFamily::from_resources(resources, id).unwrap()).unwrap()
}
fn strip(width: usize, height: usize) -> Strip
{
    Strip::new(width, height, bitmap(&family(0, false), 0)).unwrap()
}
fn inside(rect: Rect, x: usize, y: usize) -> bool
{
    x >= rect.left as usize
    && x < rect.right as usize
    && y >= rect.top as usize
    && y < rect.bottom as usize
}

#[test]
fn first_row_uses_retained_original_geometry_and_overflow_remains_visible_without_covering_panel()
{
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("data/extension-startup/reference.json")).unwrap();
    let layout = Layout::new(640, 480).unwrap();
    for ordinal in 0..reference["observed"]["visible_icons"].as_u64().unwrap() as usize
    {
        let rect = layout.bounds(ordinal).unwrap();
        assert_eq!(rect, Rect::new(8 + ordinal as i16 * 40, 440, 40 + ordinal as i16 * 40, 472));
    }
    assert_eq!(Layout::new(800, 600).unwrap().bounds(0).unwrap(), Rect::new(8, 560, 40, 592));
    assert_eq!(layout.bounds(15).unwrap(), Rect::new(8, 400, 40, 432));
    let mut count = 0;
    while let Ok(rect) = layout.bounds(count)
    {
        assert!(rect.left >= 8 && rect.right <= 632 && rect.top >= 334 && rect.bottom <= 472);
        count += 1;
    }
    assert_eq!(layout.bounds(count), Err(Error::Space));
    assert!(Error::Space.to_string().contains("larger display"));
    assert!(Layout::new(0, 480).unwrap_err().to_string().contains("framebuffer"));
}

#[test]
fn retained_package_bitmap_matches_public_icon_services_and_only_its_cell_is_painted()
{
    let package = family(128, true);
    let mut strip = strip(640, 480);
    let mut actual = vec![0x123456; 640 * 480];
    let pixels = actual.clone();
    // The callback arms refusal after all admission. Completion and paint must
    // allocate nothing, including when the originating icon family is dropped.
    let observed = strip.attempt("Host Clipboard", Some(bitmap(&package, 128)), None, || {
                            Ok(Observe::begin(true, None))
                        })
                        .unwrap()
                        .unwrap();
    let bounds = strip.paint_pending(&mut actual).unwrap().unwrap();
    let attempts = observed.finish();
    assert_eq!(attempts, 0, "completion/paint allocated after callback admission");
    assert_eq!(strip.view(0).unwrap().source, Source::Package);
    assert_eq!(strip.view(0).unwrap().outcome, Outcome::Loaded);
    assert_eq!(strip.paint_pending(&mut actual).unwrap(), None);
    let mut expected = pixels.clone();
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &encode(&package).unwrap(), |services| {
        let mut surface = mac_abi::Surface { pixels: expected.as_mut_ptr(), width: 640, height: 480, origin: Point::default(), clip: Rect::new(0, 0, 640, 480) };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        SurfacePort::new(connection).plot_icon(ResourceScope::Application, 128, bounds, IconFlags::default()).unwrap();
    }).unwrap();
    assert_eq!(actual, expected);
    for y in 0..480
    {
        for x in 0..640
        {
            if !inside(bounds, x, y)
            {
                assert_eq!(actual[y * 640 + x],
                           pixels[y * 640 + x],
                           "unrelated pixels changed at {x},{y}");
            }
        }
    }
}

#[test]
fn missing_artwork_uses_theme_refusal_draws_x_and_failed_presentation_retries_exact_outcome()
{
    let mut strip = strip(640, 480);
    let mut actual = vec![0x123456; 640 * 480];
    let before = actual.clone();
    let result =
        strip.attempt("Host Clipboard", None, None, || Err::<(), _>(mac_abi::NOT_SUPPORTED))
             .unwrap();
    assert_eq!(result, Err(mac_abi::NOT_SUPPORTED));
    let view = strip.view(0).unwrap();
    assert_eq!(view.source, Source::Theme);
    assert_eq!(view.outcome, Outcome::Refused(Refusal::Initialization(mac_abi::NOT_SUPPORTED)));
    let Outcome::Refused(cause) = view.outcome
    else
    {
        panic!("refusal was invented as loaded")
    };
    assert!(cause.cause().contains("channel") && cause.action().contains("QEMU"));
    let mut wrong = [0x123456; 10];
    assert_eq!(strip.paint_pending(&mut wrong), Err(Error::Frame));
    assert_eq!(wrong, [0x123456; 10]);
    let mut called = false;
    assert_eq!(strip.attempt("must wait", None, None, || {
                        called = true;
                        Ok(())
                    }),
               Err(Error::PaintPending));
    assert!(!called);
    let observed = Observe::begin(true, None);
    let bounds = strip.paint_pending(&mut actual).unwrap().unwrap();
    let attempts = observed.finish();
    assert_eq!(attempts, 0);
    assert_eq!(actual[(bounds.top as usize + 14) * 640 + bounds.left as usize + 14],
               0,
               "refused extension lacks its X");
    assert_eq!(actual[(bounds.top as usize + 13) * 640 + bounds.left as usize + 15],
               0xffffff,
               "failure X lacks its high-contrast halo");
    for y in 0..480
    {
        for x in 0..640
        {
            if !inside(bounds, x, y)
            {
                assert_eq!(actual[y * 640 + x], before[y * 640 + x]);
            }
        }
    }
    assert_eq!(strip.view(0).unwrap().name, "Host Clipboard");
}

#[test]
fn admission_failure_and_interrupted_callback_never_publish_a_loaded_outcome()
{
    let mut strip = strip(640, 480);
    let mut called = false;
    let observed = Observe::begin(true, None);
    let result = strip.attempt("refused storage", None, None, || {
                          called = true;
                          Ok(())
                      });
    let attempts = observed.finish();
    assert_eq!(result, Err(Error::Memory));
    assert!(attempts > 0);
    assert!(!called);
    assert_eq!(strip.len(), 0);
    assert!(Error::Memory.to_string().contains("System memory"));
    let abandoned =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                     strip.attempt::<()>("interrupted test callback",
                                                         None,
                                                         None,
                                                         || panic!("host-only unwind probe"))
                                 }));
    assert!(abandoned.is_err());
    assert_eq!(strip.view(0).unwrap().outcome, Outcome::Refused(Refusal::Interrupted));
    let mut pixels = vec![0x123456; 640 * 480];
    strip.paint_pending(&mut pixels).unwrap();
    assert_eq!(strip.attempt("retry", None, None, || Ok(())).unwrap(), Ok(()));
    assert_eq!(strip.view(1).unwrap().outcome, Outcome::Loaded);
    // This verifies the helper's owned outcome on a host unwind. It does not
    // claim recovery from arbitrary privileged module faults (M-D04).
}
