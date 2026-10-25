//! Power policy through the public SDK, real resident selection and disk flushes.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_power_interface::Action;
use mac_system::testing::{DiskFaults, power as hardware, with_file_disk, with_resource_forks};
use mac_toolbox::{Error, System,
                  files::{DataFileSpec, ROOT_ID},
                  power::{Capabilities, Stage}};
fn device(capabilities: u64) -> hardware::Device
{
    hardware::Device { version: 1,
                       capabilities,
                       status: 0 }
}
fn session(run: impl FnOnce(&System<'_>))
{
    let empty = mac_formats::resource::encode(&[]).unwrap();
    with_resource_forks(&empty, &empty, |services| {
        run(&unsafe { System::from_raw(services) }.unwrap())
    }).unwrap();
}
#[test]
fn absence_and_contract_mismatches_are_typed_actionable_and_leave_services_usable()
{
    session(|system| {
        assert_eq!(system.power().capabilities().unwrap(),
                   Capabilities { shutdown: false,
                                  restart:  false });
        let missing = system.power().shutdown().unwrap_err();
        assert_eq!(missing.stage, Stage::Selection);
        assert_eq!(missing.cause, Error::NotSupported);
        assert!(missing.to_string().contains("Install a compatible driver"));
        assert!(hardware::triggered().is_empty());
        hardware::install(&[hardware::Device { version: 2,
                                               ..device(3) }]);
        let mismatch = system.power().restart().unwrap_err();
        assert_eq!(mismatch.cause, Error::BadAbi);
        assert!(mismatch.to_string().contains("matching driver"));
        hardware::install(&[device(3)]);
        system.power().restart().unwrap();
        assert_eq!(hardware::triggered(), [Action::Restart]);
    });
}
#[test]
fn system_selects_any_provider_supporting_the_action_and_the_matching_version()
{
    session(|system| {
        hardware::install(&[hardware::Device { version: 2,
                                               ..device(3) },
                            device(1),
                            device(2)]);
        assert_eq!(system.power().capabilities().unwrap(),
                   Capabilities { shutdown: true,
                                  restart:  true });
        system.power().shutdown().unwrap();
        system.power().restart().unwrap();
        assert_eq!(hardware::triggered(), [Action::Shutdown, Action::Restart]);
        hardware::install(&[device(1)]);
        let refused = system.power().restart().unwrap_err();
        assert_eq!(refused.stage, Stage::Selection);
        assert_eq!(refused.cause, Error::NotSupported);
        assert!(refused.to_string().contains("restart"));
        assert!(hardware::triggered().is_empty());
    });
}
#[test]
fn device_failure_retains_its_identity_and_the_next_action_can_retry()
{
    session(|system| {
        hardware::install(&[hardware::Device { status: mac_toolbox::abi::IO_ERROR,
                                               ..device(3) }]);
        let failure = system.power().shutdown().unwrap_err();
        assert_eq!(failure.stage, Stage::Device);
        assert_eq!(failure.cause, Error::Io);
        assert!(failure.to_string().contains("power device failed"));
        system.power().restart().unwrap();
        assert_eq!(hardware::triggered(), [Action::Shutdown, Action::Restart]);
    });
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Disk
{
    directory: PathBuf,
    path:      PathBuf
}
impl Disk
{
    fn new() -> Self
    {
        let directory=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../results/system7")
            .join(format!("power-sdk-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("disk.hfsb");
        let mut image = hfs_b::Image::create(&path, 16 * 1024 * 1024).unwrap();
        let partition = image.partition();
        mac_hfs_b::format(&mut image,partition,mac_hfs_b::FormatOptions {name:"Power",created:mac_hfs_b::time::Timestamp(4_600_000_000_000_000),uuid:[91;16]},Default::default()).unwrap();
        Self { directory,
               path }
    }
}
impl Drop for Disk
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
#[test]
fn failed_flush_does_not_trigger_hardware_and_a_retry_really_flushes()
{
    let disk = Disk::new();
    let (_, activity) =
        with_file_disk(&disk.path,
                       false,
                       DiskFaults { flush_at: Some(0),
                                    ..Default::default() },
                       |services| {
                           let system = unsafe { System::from_raw(services) }.unwrap();
                           hardware::install(&[device(3)]);
                           let failure = system.power().shutdown().unwrap_err();
                           assert_eq!(failure.stage, Stage::Storage);
                           assert_eq!(failure.cause, Error::Io);
                           assert!(failure.to_string().contains("no power request was issued"));
                           assert!(hardware::triggered().is_empty());
                           assert_eq!(system.files().volume_at(0).unwrap().name(), "Power");
                           system.power().restart().unwrap();
                           assert_eq!(hardware::triggered(), [Action::Restart]);
                       }).unwrap();
    assert_eq!(activity.flushes, 2);
    assert_eq!(activity.writes, 0);
}
#[test]
fn unfinished_writes_and_running_apps_refuse_power_then_release_cleanly()
{
    let disk = Disk::new();
    let (_, activity) = with_file_disk(&disk.path, false, DiskFaults::default(), |services| {
                            let system = unsafe { System::from_raw(services) }.unwrap();
                            hardware::install(&[device(3)]);
                            let volume = system.files().volume_at(0).unwrap().id;
                            let writer =
                                system.files()
                                      .create_data(DataFileSpec { volume,
                                                                  parent: ROOT_ID,
                                                                  name: "Draft",
                                                                  replacing: None,
                                                                  file_type: (*b"TEXT").into(),
                                                                  creator: (*b"TEST").into() },
                                                   0)
                                      .unwrap();
                            let busy = system.power().shutdown().unwrap_err();
                            assert_eq!(busy.stage, Stage::Storage);
                            assert_eq!(busy.cause, Error::ServiceBusy);
                            assert!(busy.to_string().contains("Wait"));
                            assert!(hardware::triggered().is_empty());
                            drop(writer);
                            hardware::applications_running(true);
                            let apps = system.power().restart().unwrap_err();
                            assert_eq!(apps.stage, Stage::Applications);
                            assert!(apps.to_string().contains("Save your documents"));
                            assert!(hardware::triggered().is_empty());
                            hardware::applications_running(false);
                            system.power().shutdown().unwrap();
                            assert_eq!(hardware::triggered(), [Action::Shutdown]);
                        }).unwrap();
    assert_eq!(activity.flushes, 1);
}
#[test]
fn malformed_power_wire_never_reaches_a_peripheral()
{
    session(|system| {
        hardware::install(&[device(3)]);
        for mut call in [mac_toolbox::abi::power::PowerCall { operation: u32::MAX,
                                                              ..Default::default() },
                         mac_toolbox::abi::power::PowerCall { reserved: 1,
                                                              ..Default::default() }]
        {
            assert_eq!(system.client().power(&mut call).unwrap_err(), Error::BadArgument);
        }
        assert!(hardware::triggered().is_empty());
        system.power().restart().unwrap();
        assert_eq!(hardware::triggered(), [Action::Restart]);
    });
}

#[test]
#[ignore = "requires a rebuilt private image and graphical QEMU input"]
fn external_power_driver_handles_finder_restart_shutdown_and_refusal_recovery()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/power-ui-{}", std::process::id()));
    let mut command = std::process::Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_power.py")).arg("--output").arg(&output);
    if std::env::var_os("SYSTEM7_POWER_WINDOWS").is_some()
    {
        command.arg("--windows-headless");
    }
    let result = command.output().unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    for stage in report["stages"].as_array().unwrap()
    {
        let serial = fs::read_to_string(output.join(stage["serial"].as_str().unwrap())).unwrap();
        for bad in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:",
                    "MODULE_REJECTED name=Qemu Power Device"]
        {
            assert!(!serial.contains(bad), "{serial}");
        }
        if stage["scenario"] == "missing-driver"
        {
            assert_eq!(stage["guest_shutdown"], false);
            assert!(stage["exit_code"].is_null());
            assert!(serial.contains("FINDER_POWER_CAPABILITIES shutdown=false restart=false"));
            assert!(!serial.contains("MODULE_PUBLISHED name=Qemu Power Device"));
            assert!(!serial.contains("command=73687574"), "disabled action reached Finder");
        }
        else
        {
            assert_eq!(stage["guest_shutdown"], true);
            assert_eq!(stage["exit_code"], 0);
            assert!(serial.contains("MODULE_PUBLISHED name=Qemu Power Device"));
            assert!(serial.contains("FINDER_POWER_CAPABILITIES shutdown=true restart=true"));
            if stage["scenario"] == "restart"
            {
                assert_eq!(serial.matches("System RISC Boot ROM v1").count(),
                           2,
                           "restart did not return through the ROM");
                assert_eq!(serial.matches("DESKTOP_READY:").count(),
                           2,
                           "restart did not reach the new Finder");
                assert!(serial.contains("command=72657374"));
            }
            else
            {
                assert!(serial.contains("Save your documents"));
                assert!(serial.contains("FINDER_ALERT_DISMISSED"));
                assert!(serial.contains("PROCESS_EXIT app="));
            }
            assert!(serial.contains("command=73687574"));
        }
    }
}
