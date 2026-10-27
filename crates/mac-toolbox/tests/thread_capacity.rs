//! Public SDK acceptance for dynamic live process/thread capacity.
#![cfg(not(target_arch = "riscv64"))]
use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires an optimized private image and QEMU"]
fn more_than_eight_apps_receive_input_and_reuse_reclaimed_stacks()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("thread-capacity-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_thread_capacity.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capacity transport failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(!serial.contains("THREAD_CAPACITY_FAIL"), "{serial}");
    assert!(!serial.contains("CRASH_REPORT_READY"), "{serial}");
    for phase in 0..2
    {
        assert!(serial.contains(&format!("THREAD_CAPACITY_LIVE phase={phase} apps=10\n")),
                "{serial}");
    }
    let rows: Vec<_> =
        serial.lines().filter(|line| line.starts_with("THREAD_CAPACITY_WORKER ")).collect();
    assert_eq!(rows.len(), 18, "{serial}");
    for field in ["app", "thread", "image", "window"]
    {
        let values: BTreeSet<_> =
            rows.iter()
                .map(|line| {
                    line.split_whitespace()
                        .find_map(|part| part.strip_prefix(&format!("{field}=")))
                        .unwrap()
                })
                .collect();
        assert_eq!(values.len(), 18, "reused or foreign {field} identity: {serial}");
    }
    // Within each concurrently running cohort, mutable globals reside at
    // different actual addresses. A second incarnation may reuse real backing.
    for cohort in rows.chunks_exact(9)
    {
        let addresses: BTreeSet<_> =
            cohort.iter()
                  .map(|line| {
                      line.split_whitespace().find_map(|part| part.strip_prefix("global=")).unwrap()
                  })
                  .collect();
        assert_eq!(addresses.len(), 9, "writable app images alias: {serial}");
    }
    // Observe real native storage, not just growable thread bookkeeping.
    // Each worker's stack must be in discovered RAM, outside metadata, with
    // disjoint storage while concurrent. A retired cohort returns reusable
    // physical backing even though all object identities stay fresh.
    let number = |line: &str, name: &str| -> u64 {
        let text = line.split_whitespace().find_map(|part| part.strip_prefix(name)).unwrap();
        if let Some(hex) = text.strip_prefix("0x")
        {
            u64::from_str_radix(hex, 16).unwrap()
        }
        else
        {
            text.parse().unwrap()
        }
    };
    let banks: Vec<_> = serial.lines()
                              .filter(|line| line.starts_with("PAGE_BANK "))
                              .map(|line| {
                                  let start = number(line, "base=");
                                  (start, start + number(line, "bytes="))
                              })
                              .collect();
    assert!(!banks.is_empty(), "resident physical pool was not initialized: {serial}");
    let threads: Vec<_> =
        serial.lines().filter(|line| line.starts_with("PROCESS_THREAD ")).collect();
    let mut cohorts = Vec::new();
    for cohort in rows.chunks_exact(9)
    {
        let mut ranges = Vec::new();
        for row in cohort
        {
            let id = number(row, "thread=");
            let stack = threads.iter().find(|line| number(line, "thread=") == id).unwrap();
            let top = number(stack, "stack=");
            let bytes = number(stack, "stack_bytes=");
            assert!(bytes > 0 && bytes.is_power_of_two());
            let base = top.checked_sub(bytes).unwrap();
            assert!(banks.iter().any(|(start, end)| *start <= base && top <= *end),
                    "stack is outside the actual page pools: {stack}");
            assert!(base.is_multiple_of(bytes), "stack has no buddy block alignment: {stack}");
            for &(old_base, old_top) in &ranges
            {
                assert!(top <= old_base || old_top <= base, "concurrent stacks alias: {serial}");
            }
            ranges.push((base, top));
        }
        cohorts.push(ranges);
    }
    assert!(cohorts[0].iter().any(|range| cohorts[1].contains(range)),
            "second cohort did not reuse any genuinely reclaimed page-backed stack: {serial}");
    assert_eq!(serial.lines().filter(|line| line.starts_with("THREAD_CAPACITY_ACK ")).count(),
               18,
               "{serial}");
    assert_eq!(report["input"].as_array().unwrap().len(), 18);
    assert!(serial.contains("THREAD_CAPACITY_PASS apps=10 incarnations=18 native-ids=passed \
                             private-globals=passed input=passed reuse=passed"),
            "{serial}");
}
