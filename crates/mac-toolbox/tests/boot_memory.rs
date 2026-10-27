//! Physical boot admission through the real ROM, disk and resident System.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::Path,
          process::{Child, Command, Stdio},
          thread,
          time::{Duration, Instant}};
struct Guest(Child);
impl Drop for Guest
{
    fn drop(&mut self)
    {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn boot(output: &Path, mib: u64) -> String
{
    let state = output.join(mib.to_string());
    fs::create_dir_all(&state).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let dist = mac_build::paths::dist_dir(&root);
    let serial = state.join("serial.log");
    let diagnostics = state.join("qemu.log");
    let log = fs::File::create(&diagnostics).unwrap();
    let platform = mac_build::platform::current();
    let mut command = Command::new("qemu-system-riscv64");
    command.args(["-machine",
                  "virt",
                  "-cpu",
                  &platform.qemu_cpu,
                  "-m",
                  &format!("{mib}M"),
                  "-smp",
                  "1",
                  "-bios",
                  "none",
                  "-drive"])
           .arg(format!("if=pflash,unit=0,format=raw,readonly=on,file={}",
                        dist.join("bootrom.bin").display()))
           .arg("-drive")
           .arg(format!("if=none,id=startup,format=qcow2,readonly=on,file={}",
                        dist.join("system7.qcow2").display()))
           .args(["-global", "virtio-mmio.force-legacy=false", "-device",
                  "virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0", "-device", "ramfb",
                  "-device", "virtio-keyboard-device", "-device", "virtio-tablet-device",
                  "-display", "none", "-nic", "none", "-monitor", "none", "-serial"])
           .arg(format!("file:{}", serial.display()))
           .stdin(Stdio::null())
           .stdout(log.try_clone().unwrap())
           .stderr(log);
    fs::write(state.join("command.txt"), format!("{command:?}\n")).unwrap();
    let mut guest = Guest(command.spawn().expect("private QEMU boot"));
    let deadline = Instant::now() + Duration::from_secs(45);
    loop
    {
        let text = fs::read_to_string(&serial).unwrap_or_default();
        // Serial is streamed; preserve the entire refusal before the owned
        // guest is stopped. A prefix alone can truncate the useful cause.
        if text.split_inclusive('\n').any(|line| {
                                         line.ends_with('\n')
                                         && (line.contains("DESKTOP_READY")
                                             || line.contains("BOOT_MEMORY_REJECTED:"))
                                     })
        {
            return text;
        }
        assert!(guest.0.try_wait().unwrap().is_none(),
                "QEMU stopped:\n{text}\n{}",
                fs::read_to_string(&diagnostics).unwrap_or_default());
        assert!(Instant::now() < deadline,
                "boot timeout for {mib} MiB:\n{text}\n{}",
                fs::read_to_string(&diagnostics).unwrap_or_default());
        thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "requires an optimized current disk/ROM and private QEMU"]
fn discovered_ram_drives_boot_info_and_small_machines_are_refused_before_heap_use()
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("boot-memory-{}", std::process::id()));
    fs::create_dir_all(&output).unwrap();
    let small = boot(&output, 64);
    assert!(small.contains("discovered RAM is smaller than the platform minimum; increase VM \
                            RAM or select another platform"),
            "{small}");
    assert!(!small.contains("TRAPS_READY"), "rejected machine entered System: {small}");
    let insufficient_metadata = boot(&output, 128);
    assert!(insufficient_metadata.contains("needs")
            && insufficient_metadata.contains("contiguous bytes of reserved buddy metadata"),
            "{insufficient_metadata}");
    assert!(!insufficient_metadata.contains("TRAPS_READY"),
            "pool refusal entered normal services: {insufficient_metadata}");
    for mib in [512u64, 8192]
    {
        let text = boot(&output, mib);
        assert!(text.contains("DESKTOP_READY"), "{mib} MiB failed:\n{text}");
        assert!(text.contains(&format!("BOOT_RAM base=0x80000000 bytes={} banks=1",
                                       mib * 1024 * 1024)),
                "{text}");
        assert!(text.contains(&format!("primary_bytes={}", mib * 1024 * 1024)),
                "BootInfo lost discovered RAM: {text}");
        let field = |line: &str, name: &str| -> u64 {
            let value = line.split_whitespace().find_map(|part| part.strip_prefix(name)).unwrap();
            if let Some(hex) = value.strip_prefix("0x")
            {
                u64::from_str_radix(hex, 16).unwrap()
            }
            else
            {
                value.parse().unwrap()
            }
        };
        let admission = text.lines()
                            .find(|line| line.starts_with("BOOT_TRANSLATION "))
                            .expect("no actual ROM translation admission");
        let activated = text.lines()
                            .find(|line| line.starts_with("MMU_READY "))
                            .expect("no actual resident root activation");
        assert!(activated.contains("mode=sv39") && activated.contains("guards=2"), "{activated}");
        let actual = field(activated, "actual=");
        let kernel = field(activated, "kernel_root=");
        let firmware = field(activated, "firmware_root=");
        assert_eq!(actual >> 60, 8, "actual hardware translation mode: {activated}");
        assert_eq!((actual & ((1u64 << 44) - 1)) << 12,
                   kernel,
                   "active PPN differs from owned root: {activated}");
        for physical in [kernel, firmware]
        {
            assert!(physical.is_multiple_of(4096)
                    && (0x80000000..0x80000000 + mib * 1024 * 1024).contains(&physical),
                    "root lacks discovered physical backing: {activated}");
            assert_ne!(physical,
                       field(admission, "root="),
                       "resident retained a firmware-owned boot root: {activated}");
        }
        assert_ne!(kernel, firmware,
                   "firmware and System need separately owned trees: {activated}");
        for failure in ["BOOT_MEMORY_REJECTED:", "SYSTEM_PANIC:", "SYSTEM_TRAP:",
                        "SYSTEM_ALLOCATION_FAILED:", "BOOT_TRAP:"]
        {
            assert!(!text.contains(failure), "{text}");
        }
    }
}

#[test]
fn scratch_handoff_requires_the_selected_reserved_range_and_explains_stale_bootstrap_contracts()
{
    use mac_toolbox::abi::{BootMemoryRange,
                           boot::{Memory, MemoryError}};
    let stale_header =
        mac_toolbox::abi::Header { version: mac_toolbox::abi::ABI_VERSION,
                                   size:    (core::mem::size_of::<mac_toolbox::abi::BootInfo>()
                                             - core::mem::size_of::<BootMemoryRange>())
                                            as u32 };
    assert!(!stale_header.supports::<mac_toolbox::abi::BootInfo>());
    let scratch = BootMemoryRange { base:   0x86a81000,
                                    length: 0x57f000 };
    let ram = BootMemoryRange { base:   0x80000000,
                                length: 128 * 1024 * 1024 };
    let memory = Memory { ram:          &[ram],
                          reservations: &[scratch] };
    assert!(memory.display_handoff(4096, ram, scratch, scratch).is_ok());
    let stale = BootMemoryRange { base: 0x87800000,
                                  ..scratch };
    let error = memory.display_handoff(4096, ram, stale, scratch).unwrap_err();
    assert_eq!(error, MemoryError::PlatformMismatch);
    assert!(error.to_string().contains("rebuild ROM and disk together"));
    let memory = Memory { ram:          &[ram],
                          reservations: &[BootMemoryRange { length: 0x120000,
                                                            ..scratch }] };
    let error = memory.display_handoff(4096, ram, scratch, scratch).unwrap_err();
    assert_eq!(error, MemoryError::MissingReservation);
    assert!(error.to_string().contains("not reserved"));
    assert!(error.to_string().contains("rebuild ROM and disk together"));
}
