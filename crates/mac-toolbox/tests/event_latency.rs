//! Public SDK correctness plus measured dispatch distributions on real RV64.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn waits_preserve_events_and_wake_under_real_application_contention()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let windows = std::env::var_os("SYSTEM7_EVENT_LATENCY_WINDOWS").is_some();
    let output =
        root.join("results/system7").join(format!("event-latency-{}-{}",
                                                  if windows { "windows" } else { "linux" },
                                                  std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_event_latency.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if windows
    {
        command.arg("--windows-headless");
    }
    let transport = command.output().unwrap();
    assert!(transport.status.success(),
            "event wait transport failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    assert!(!serial.contains("EVENT_LATENCY_FAIL "), "guest assertion failed:\n{serial}");
    for marker in ["EVENT_LATENCY_WAIT_CONTRACT_PASS cases=6\n",
                   "EVENT_LATENCY_POSTED_NULL_PASS\n", "EVENT_LATENCY_POINTER_PASS\n",
                   "EVENT_LATENCY_PHYSICAL_PASS scenario=IDLE transitions=8\n",
                   "EVENT_LATENCY_PHYSICAL_PASS scenario=BUSY transitions=8\n",
                   "EVENT_LATENCY_MASKED_KEY_PASS transitions=2\n",
                   "EVENT_LATENCY_USERSPACE_PASS\n"]
    {
        assert!(serial.contains(marker), "missing {marker}:\n{serial}");
    }
    let spin_start = serial.find("PROCESS_SPIN_STARTED\n").expect("spinner never ran");
    let progress = serial.find("EVENT_LATENCY_PROGRESS_DURING_SPIN\n")
                         .expect("observer never received input during contention");
    let spin_end = serial.find("PROCESS_SPIN_FINISHED\n").expect("spinner never completed");
    assert!(spin_start < progress && progress < spin_end,
            "input must get through during the real no-trap/no-yield spinner interval:\n{serial}");
    let blocked = report["blocked_samples"].as_array().unwrap();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0]["timeout_ticks"], 6);
    assert!(blocked[0]["reschedules"].as_u64().unwrap() <= 8,
            "waiting application remained in the runnable rotation");
    let distributions = report["distributions"].as_array().unwrap();
    assert_eq!(distributions.len(), 2);
    for (sample, scenario) in distributions.iter().zip(["single-app", "three-apps-spinner"])
    {
        assert_eq!(sample["scenario"], scenario);
        assert_eq!(sample["samples"], 64);
        let ordered: Vec<_> =
            ["p50_ns", "p95_ns", "p99_ns", "max_ns"].into_iter()
                                                    .map(|key| sample[key].as_u64().unwrap())
                                                    .collect();
        assert!(ordered.windows(2).all(|pair| pair[0] <= pair[1]));
    }
    let strict = std::env::var_os("SYSTEM7_STRICT_EVENT_LATENCY").is_some();
    if report["physical_timing_available"] == true
    {
        assert_eq!(report["physical_delivery_ns"].as_array().unwrap().len(), 16);
        if strict
        {
            for value in report["physical_delivery_ns"].as_array().unwrap()
            {
                assert!(value.as_u64().unwrap() < 1_000_000,
                        "decoded input exceeded requested1ms delivery budget: {report}");
            }
        }
    }
    if strict
    {
        assert_eq!(report["physical_timing_available"], true,
                   "strict physical timing requires an event-trace image");
        for sample in distributions
        {
            assert!(sample["max_ns"].as_u64().unwrap() < 1_000_000,
                    "queued dispatch exceeded requested1ms budget: {sample}");
        }
    }
    let acceptance = serde_json::json!({"status":"passed", "native_wait_contracts":"passed", "physical_transitions_exactly_once":18, "background_no_yield_spinner_progress":"passed", "latency_target_ns":1_000_000u64, "latency_threshold_enforced":strict, "timing_limits":"Guest post-to-wait-return samples; physical trace starts at driver decoding, not host arrival; presented frame latency excluded. Host scheduling and emulation are not a hard real-time guarantee.", "capture":report});
    let bytes = serde_json::to_vec_pretty(&acceptance).unwrap();
    fs::write(output.join("rust-acceptance.json"), &bytes).unwrap();
    fs::write(root.join("results/system7").join(if windows
                                                {
                                                    "event-latency-windows-checks.json"
                                                }
                                                else
                                                {
                                                    "event-latency-checks.json"
                                                }),
              &bytes).unwrap();
}
