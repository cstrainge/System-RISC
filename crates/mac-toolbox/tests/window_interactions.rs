//! Real userspace Window Manager acceptance. The capture transport only drives
//! virtio input and records returned state/framebuffers; assertions live here.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;

struct Frame
{
    width:  usize,
    height: usize,
    rgb:    Vec<u8>
}
impl Frame
{
    fn read(path: &Path) -> Self
    {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut at = 0;
        fn token<'a>(bytes: &'a [u8], at: &mut usize) -> &'a [u8]
        {
            loop
            {
                while bytes.get(*at).is_some_and(u8::is_ascii_whitespace)
                {
                    *at += 1;
                }
                if bytes.get(*at) != Some(&b'#')
                {
                    break;
                }
                while bytes.get(*at).is_some_and(|b| *b != b'\n')
                {
                    *at += 1;
                }
            }
            let start = *at;
            while bytes.get(*at).is_some_and(|b| !b.is_ascii_whitespace())
            {
                *at += 1;
            }
            &bytes[start..*at]
        }
        assert_eq!(token(&bytes, &mut at), b"P6");
        let width = std::str::from_utf8(token(&bytes, &mut at)).unwrap().parse::<usize>().unwrap();
        let height = std::str::from_utf8(token(&bytes, &mut at)).unwrap().parse::<usize>().unwrap();
        assert_eq!(token(&bytes, &mut at), b"255");
        assert!(bytes[at].is_ascii_whitespace());
        at += if bytes.get(at..at + 2) == Some(b"\r\n") { 2 } else { 1 };
        let rgb = bytes[at..].to_vec();
        assert_eq!(rgb.len(), width * height * 3);
        Self { width,
               height,
               rgb }
    }

    fn pixel(&self, x: usize, y: usize) -> &[u8]
    {
        &self.rgb[(y * self.width + x) * 3..(y * self.width + x + 1) * 3]
    }

    fn assert_same(&self, other: &Self, reason: &str)
    {
        assert_eq!((self.width, self.height), (other.width, other.height));
        let different = self.rgb
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .zip(other.rgb.as_chunks::<3>().0.iter())
                            .filter(|(a, b)| a != b)
                            .count();
        assert_eq!(different, 0, "{reason}: differing RGB pixels");
    }
}

#[test]
#[ignore = "requires freshly built System/Finder disk image and qemu-system-riscv64; run after \
            cargo xtask image"]
fn real_userspace_window_tracking_commits_on_release_and_cancels_cleanly()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("window-tracking-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_window_tracking.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .expect("start isolated capture");
    assert!(result.status.success(),
            "capture transport failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"],
               "readonly fixture disk changed");
    let observations = report["observations"].as_array().unwrap();
    let get = |name: &str| {
        observations.iter()
                    .find(|o| o["name"] == name)
                    .unwrap_or_else(|| panic!("missing observation {name}"))
    };
    let frame = |name: &str| Frame::read(&output.join(get(name)["frame"].as_str().unwrap()));
    let original_id = get("baseline")["state"]["front"].clone();
    let background_id = get("background-baseline")["state"]["front"].clone();
    let bounds = |name: &str, expected: &str| {
        let state = &get(name)["state"];
        assert_eq!(state["first"], expected, "{name} window bounds");
        assert_eq!(state["windows"], "1", "{name} window count");
        assert_eq!(state["front"], original_id, "{name} z order");
    };
    for name in ["baseline", "drag-held", "drag-outside-held", "drag-cancelled", "drag-reentered"]
    {
        bounds(name, "8,46,412,234");
    }
    assert_eq!(get("drag-held")["state"]["updates"],
               get("baseline")["state"]["updates"],
               "held drag emitted an application update");
    let baseline = frame("baseline");
    assert_eq!((baseline.width, baseline.height), (640, 480));
    baseline.assert_same(&frame("drag-cancelled"), "cancelled drag changed composed scene");
    bounds("drag-committed", "68,79,472,267");
    assert_eq!(get("drag-committed")["state"]["action"], "DRAG");
    bounds("grow-held", "68,79,472,267");
    assert_eq!(get("grow-held")["state"]["updates"],
               get("drag-committed")["state"]["updates"],
               "held grow emitted an application update");
    bounds("grow-committed", "68,79,412,237");
    assert_eq!(get("grow-committed")["state"]["action"], "RESIZE");
    bounds("roundtrip", "8,46,412,234");
    baseline.assert_same(&frame("roundtrip"), "drag/grow roundtrip left stale pixels");
    for name in ["close-held", "close-left", "close-reentered", "close-cancelled", "zoom-held",
                 "zoom-left", "zoom-reentered", "zoom-cancelled"]
    {
        bounds(name, "8,46,412,234");
    }
    frame("close-held").assert_same(&frame("close-reentered"),
                                    "close reentry failed to restore pressed pixels");
    frame("zoom-held").assert_same(&frame("zoom-reentered"),
                                   "zoom reentry failed to restore pressed pixels");
    baseline.assert_same(&frame("close-cancelled"), "close cancellation changed pixels");
    baseline.assert_same(&frame("zoom-cancelled"), "zoom cancellation changed pixels");
    let zoomed: Vec<i16> =
        get("zoom-committed")["state"]["first"].as_str()
                                               .unwrap()
                                               .split(',')
                                               .map(|value| value.parse().unwrap())
                                               .collect();
    assert!(zoomed[0] >= 4 && zoomed[1] >= 44 && zoomed[2] <= 636 && zoomed[3] <= 458);
    assert!(zoomed[2] - zoomed[0] < 632 || zoomed[3] - zoomed[1] < 414,
            "Finder standard state ignored its small cached catalog: {zoomed:?}");
    bounds("zoom-restored", "8,46,412,234");
    baseline.assert_same(&frame("zoom-restored"), "zoom roundtrip changed pixels");
    for (name, bounds, front) in [("background-baseline", "8,46,412,234", &background_id),
                                  ("command-drag-held", "8,46,412,234", &background_id),
                                  ("command-drag-committed", "68,79,472,267", &background_id),
                                  ("background-drag-cancelled", "68,79,472,267", &background_id),
                                  ("background-drag-held", "68,79,472,267", &background_id),
                                  ("background-drag-activated", "8,46,412,234", &original_id)]
    {
        assert_eq!(get(name)["state"]["windows"], "2", "{name} count");
        assert_eq!(&get(name)["state"]["front"], front, "{name} activation/z order");
        assert_eq!(get(name)["state"]["first"], bounds, "{name} bounds");
    }
    assert_eq!(get("close-accepted")["state"]["windows"], "1");
    assert_eq!(get("close-accepted")["state"]["front"], background_id);
    assert_eq!(get("close-accepted")["state"]["first"], "0,0,0,0");
    assert_eq!(get("last-close-accepted")["state"]["windows"], "0");
    assert_eq!(get("last-close-accepted")["state"]["front"], "0");
    // Menu bar and document pixels stay fixed during the held preview. The
    // separate moving outline crosses other portions of the original content.
    let held = frame("drag-held");
    for y in 0..21
    {
        for x in 0..640
        {
            assert_eq!(baseline.pixel(x, y), held.pixel(x, y), "drag touched menu at{x},{y}");
        }
    }
    for y in 100..150
    {
        for x in 200..250
        {
            assert_eq!(baseline.pixel(x, y),
                       held.pixel(x, y),
                       "held drag moved document at{x},{y}");
        }
    }
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({"status":"passed", "assertions":"mac-toolbox/tests/window_interactions.rs", "observations":observations.len(), "transport":"isolated real RV64 userspace / public SDK / virtio input", "animation_scope":"held outline pixel oracle in separate aggregate test; exhaustive temporal sampling pending"})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires built System disk and original tracking captures; real public SDK guest \
            fixture"]
fn original_drag_and_grow_outline_pixels_match_through_public_sdk()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("window-original-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_window_tracking.py"))
                                        .args(["--probe", "--output"])
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .expect("start private userspace probe");
    assert!(result.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"]);
    let observations = report["observations"].as_array().unwrap();
    let get = |name: &str| observations.iter().find(|o| o["name"] == name).unwrap();
    let frame = |name: &str| Frame::read(&output.join(get(name)["frame"].as_str().unwrap()));
    let original: Value =
        serde_json::from_str(include_str!("data/original-window-tracking.json")).unwrap();
    let baseline = frame("baseline");
    let mut measurements = Vec::new();
    for (name, mask) in [("drag", include_bytes!("data/original-window-drag-mask.bin").as_slice()),
                         ("grow", include_bytes!("data/original-window-grow-mask.bin").as_slice())]
    {
        let held = frame(&format!("{name}-held"));
        let case = &original["cases"][name];
        let mut changed = 0;
        let mut wrong = Vec::new();
        for y in 0..480
        {
            for x in 0..640
            {
                let index = y * 640 + x;
                let expected = mask[index / 8] & (0x80 >> (index % 8)) != 0;
                let actual = baseline.pixel(x, y) != held.pixel(x, y);
                if actual
                {
                    changed += 1;
                }
                if expected != actual && wrong.len() < 20
                {
                    wrong.push((x, y, expected, actual));
                }
            }
        }
        assert!(wrong.is_empty(), "{name} original outline mask differs: {wrong:?}");
        assert_eq!(changed, case["changed_pixels"].as_u64().unwrap() as usize);
        let mut common_source_pixels = 0;
        for item in case["observed_rgb_pairs"].as_array().unwrap()
        {
            let x = item[0].as_u64().unwrap() as usize;
            let y = item[1].as_u64().unwrap() as usize;
            let before = item[2].as_u64().unwrap() as u32;
            let after = item[3].as_u64().unwrap() as u32;
            let before = [(before >> 16) as u8, (before >> 8) as u8, before as u8];
            let after = [(after >> 16) as u8, (after >> 8) as u8, after as u8];
            // Original application has control samples; this minimal public-SDK
            // probe has white content. Compare color only for identical source
            // pixels; the complete changed-pixel geometry is checked above.
            if baseline.pixel(x, y) == before
            {
                common_source_pixels += 1;
                assert_eq!(held.pixel(x, y), after, "{name} indexed XOR at{x},{y}");
            }
        }
        assert!(common_source_pixels > changed / 2, "too little common original content");
        assert_eq!(get(&format!("{name}-held"))["state"]["first"], "100,100,540,400");
        let expected = case["committed_bounds"].as_array()
                                               .unwrap()
                                               .iter()
                                               .map(ToString::to_string)
                                               .collect::<Vec<_>>()
                                               .join(",");
        assert_eq!(get(&format!("{name}-committed"))["state"]["first"], expected);
        baseline.assert_same(&frame(&format!("{name}-restored")),
                             "original fixture roundtrip left stale pixels");
        measurements.push(serde_json::json!({"case":name,"exact_mask_pixels":640*480,"changed_pixels":changed,"common_original_rgb_pairs":common_source_pixels}));
    }
    assert_eq!(get("grow-no-motion")["state"]["first"], "100,100,540,400");
    baseline.assert_same(&frame("grow-no-motion"), "no-motion grow changed scene");
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","source":"public SDK-only guest probe", "original":"provenance/control-tracking-reference.json", "cases":measurements})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires optimized traced System image and a private QEMU guest"]
fn held_window_drag_is_a_bounded_frontbuffer_xor_with_clean_backbuffer()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("window-drag-profile-{}", std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_window_tracking.py"))
           .args(["--profile", "--output"])
           .arg(&output)
           .current_dir(&root);
    if std::env::var_os("SYSTEM7_WINDOW_WINDOWS").is_some()
    {
        command.arg("--windows-headless");
    }
    let result = command.output().expect("start isolated drag profiling transport");
    assert!(result.status.success(),
            "drag capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"]);
    let motion = &report["motion"];
    for field in
        ["scene_rev", "updates", "compositions", "menu_paints", "first", "front", "windows"]
    {
        assert_eq!(motion["state_before"][field], motion["state_after"][field],
                   "held drag changed {field}");
    }
    let bytes = |field: &str| fs::read(output.join(motion[field].as_str().unwrap())).unwrap();
    let before = bytes("back_before");
    assert_eq!(before.len(), 640 * 480 * 4);
    assert_eq!(before, bytes("back_after"), "held window drag altered the backbuffer");
    assert_eq!(motion["trace"]["overwritten"], 0);
    let rows = motion["trace"]["records"].as_array().unwrap();
    let mut pending = None;
    let mut coalesced = 0;
    let mut latency = Vec::new();
    let mut xor_times = Vec::new();
    let mut xor_pixels = Vec::new();
    let mut decoded_ticks = Vec::new();
    for row in rows
    {
        assert_ne!(row[2], 7, "drag replayed a retained scene: {row}");
        assert_ne!(row[2], 4, "held drag generated update work: {row}");
        if row[2] == 1 && row[4] == 1
        {
            if pending.is_some()
            {
                coalesced += 1;
            }
            let tick = row[1].as_u64().unwrap();
            decoded_ticks.push(tick);
            pending =
                Some((tick, row[5].as_u64().unwrap() - 1, row[6].as_u64().unwrap() - 1, false));
        }
        else if row[2] == 5 && row[4].as_u64() == Some(u64::MAX)
        {
            let pixels = row[5].as_u64().unwrap();
            assert!(pixels > 0 && pixels <= 3200, "drag swept more than its edge budget: {row}");
            xor_pixels.push(pixels);
            xor_times.push(row[6].as_u64().unwrap() * 100);
            if let Some(pending) = &mut pending
            {
                pending.3 = true;
            }
        }
        else if row[2] == 14
        {
            assert!(row[5].as_u64().unwrap() <= 256, "held drag copied scene pixels: {row}");
            if let Some((tick, left, top, xored)) = pending
            {
                let from = row[6].as_u64().unwrap();
                let to = row[7].as_u64().unwrap();
                if xored
                   && from >> 32 <= left
                   && from & 0xffff_ffff <= top
                   && to >> 32 >= left + 16
                   && to & 0xffff_ffff >= top + 16
                {
                    latency.push((row[1].as_u64().unwrap() - tick) * 100);
                    pending = None;
                }
            }
        }
    }
    assert!(pending.is_none(), "final drag feedback never reached the framebuffer");
    assert!(latency.len() >= 100, "drag made insufficient sustained presentation progress");
    assert_eq!(latency.len() + coalesced, decoded_ticks.len());
    let distribution = |values: &[u64]| {
        assert!(!values.is_empty());
        let mut v = values.to_vec();
        v.sort_unstable();
        let p = |n: usize| v[(v.len() * n).div_ceil(100) - 1];
        serde_json::json!({"samples":v.len(),"p50_ns":p(50),"p95_ns":p(95),"p99_ns":p(99),"max_ns":v.last()})
    };
    let strict = std::env::var_os("SYSTEM7_STRICT_EVENT_LATENCY").is_some();
    let gaps: Vec<_> = decoded_ticks.windows(2).map(|p| (p[1] - p[0]) * 100).collect();
    let profile = serde_json::json!({"submitted":motion["submitted"].as_array().unwrap().len(),
        "decoded":decoded_ticks.len(),"coalesced":coalesced,
        "decoded_to_outline_and_cursor":distribution(&latency),"xor":distribution(&xor_times),
        "max_xor_pixels":xor_pixels.iter().max(),"decoded_interarrival":distribution(&gaps),
        "strict":strict,"limit_ns":1_000_000,
        "limits":"Every decoded motion including the first; coalescing is counted. Guest decode to completed outline XOR and cursor copy excludes host submission/polling delay and scanout. Paused snapshots are outside the stream."});
    fs::write(output.join("drag-profile.json"),serde_json::to_vec_pretty(&profile).unwrap()).unwrap();
    if strict
    {
        assert!(latency.iter().all(|&ns| ns < 1_000_000),
                "drag exceeded strict 1ms gate: {profile}");
    }
    let observations = report["observations"].as_array().unwrap();
    let frame = |name: &str| {
        Frame::read(&output.join(observations.iter().find(|v|v["name"]==name).unwrap()["frame"].as_str().unwrap()))
    };
    frame("profile-baseline").assert_same(&frame("profile-cancelled"),
                                          "cancel left stale drag or cursor pixels");
    let held = frame("profile-held");
    held.assert_same(&frame("profile-paint-restored-held"),
                     "scene paint left damaged outline pixels");
    let painted = frame("profile-painted-held");
    let mut changed = 0;
    for y in 0..480
    {
        for x in 0..640
        {
            if held.pixel(x, y) != painted.pixel(x, y)
            {
                changed += 1;
                assert!((100..540).contains(&x) && (100..160).contains(&y),
                        "paint escaped its damaged strip: {x},{y}");
            }
        }
    }
    assert!(changed > 1000, "paint-under-drag scenario did not change real content");
    fs::write(
              output.join("rust-acceptance.json"),
              serde_json::to_vec_pretty(&serde_json::json!({
        "status":"passed","profile":profile,"backbuffer_bytes_compared":before.len(),
        "scene_replays_during_held_motion":0,"scene_paint_changed_pixels":changed})).unwrap()
    ).unwrap();
}
