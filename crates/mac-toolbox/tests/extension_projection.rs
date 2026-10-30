//! Public File Manager discovery followed by the actual resident projected
//! module reader. Unselected Finder code is never admitted as System payload.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use allocations::Observe;
use mac_formats::{macbinary::MacFile,
                  native::{self, Descriptor, Role},
                  resource::{self, Resource}};
use mac_module::{PackageKind, Spec, Target};
use mac_system::testing::{self, DiskActivity, DiskFaults, files::ModuleReadError};
use mac_toolbox::{Cnid, System};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Disk
{
    directory: PathBuf,
    path:      PathBuf
}
impl Drop for Disk
{
    fn drop(&mut self)
    {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}
fn row(kind: [u8; 4], id: i16, data: Vec<u8>) -> Resource
{
    Resource { kind,
               id,
               name: None,
               attributes: 0,
               data }
}
fn part(kind: PackageKind, id: i16) -> Vec<Resource>
{
    let descriptor = Descriptor { role:        Role::CodeResource,
                                  abi_version: native::ABI_VERSION,
                                  load_base:   0x82000000,
                                  load_limit:  0x82004000,
                                  entry:       0x82000000 };
    let initialized = vec![0x13; 12_345];
    let mut code = descriptor.encode().unwrap().to_vec();
    code.extend(native::image::encode(&descriptor, [(0, initialized.len() as u64, 5, initialized.as_slice())]).unwrap());
    code.extend_from_slice(b"RLC1\0\0\0\0");
    let hex = mac_build::package::sha256(&code);
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate()
    {
        *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).unwrap();
    }
    let spec = Spec { kind,
                      kernel_abi: mac_abi::ABI_VERSION,
                      target: Target::RVA23U64,
                      identity: *b"projected-part!!",
                      code_id: id,
                      code_digest: digest,
                      heap_bytes: 0,
                      stack_bytes: 4096,
                      name: "Projected module fixture",
                      version: "0.1.0",
                      provenance: "resident reader acceptance",
                      offers: &[],
                      dependencies: &[],
                      services: &[],
                      devices: &[] };
    let mut metadata = vec![0; spec.encoded_len().unwrap()];
    spec.encode(&mut metadata).unwrap();
    vec![row(kind.code_type(), id, code),
         row(if kind == PackageKind::Driver { *b"DABI" } else { *b"EABI" },
             if kind == PackageKind::Driver { 0 } else { id },
             metadata)]
}
impl Disk
{
    fn new(files: &[(String, [u8; 4], Vec<Resource>)]) -> Self
    {
        let base = std::env::var_os("SYSTEM7_BUILD_DIR").map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("system-risc-module-checks"));
        let directory = base.join("checks").join(format!("extension-projection-{}-{}",
                                                         std::process::id(),
                                                         NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("modules.hfsb");
        let disk = Self { directory,
                          path };
        {
            let mut image = hfs_b::Image::create(&disk.path, 16 * 1024 * 1024).unwrap();
            let partition = image.partition();
            let when = mac_hfs_b::time::Timestamp(4_600_000_000_000_000);
            mac_hfs_b::format(&mut image,
                              partition,
                              mac_hfs_b::FormatOptions { name:    "Module projection",
                                                         created: when,
                                                         uuid:    [63; 16] },
                              Default::default()).unwrap();
            let mut volume =
                mac_hfs_b::Volume::mount(&mut image, partition, Default::default()).unwrap();
            for (name, file_type, resources) in files
            {
                let file = MacFile { name:      name.as_bytes().to_vec(),
                                     file_type: *file_type,
                                     creator:   *b"TEST",
                                     flags:     0,
                                     created:   0,
                                     modified:  0,
                                     data:      vec![],
                                     resources: resource::encode(resources).unwrap() };
                hfs_b::import_macbinary_at(&mut volume, 2, &file, name, when).unwrap();
            }
        }
        disk
    }

    fn read(&self, name: &str, code_allocation: Option<usize>)
            -> (Result<Vec<Resource>, ModuleReadError>, usize, DiskActivity)
    {
        let ((result, allocations), activity) =
            testing::with_file_disk(&self.path, true, DiskFaults::default(), |services| {
                // SAFETY: the real file-backed fixture pins this service table.
                let system = unsafe { System::from_raw(services) }.unwrap();
                let volume = system.files().volume_at(0).unwrap().id;
                let file = system.files().lookup(volume, Cnid(2), name).unwrap();
                let observed = code_allocation.map(|size| Observe::begin(true, Some(size)));
                let result = testing::files::module_resources(volume.0, file.id.0);
                (result, observed.map_or(0, Observe::finish))
            }).unwrap();
        (result, allocations, activity)
    }
}

#[test]
fn complete_native_package_framing_is_checked_before_the_resident_code_projection()
{
    let system = part(PackageKind::Extension, 0);
    let finder = part(PackageKind::Extension, 1);
    let code_size = system[0].data.len();
    let mut cases = Vec::new();
    let mut code2 = system.clone();
    let mut extra = system[0].clone();
    extra.id = 2;
    code2.push(extra);
    cases.push(("code2".to_string(), *b"INIT", code2, "uncompressed IDs"));
    let mut orphan = system.clone();
    orphan.push(finder[0].clone());
    cases.push(("orphan-finder-code".to_string(), *b"INIT", orphan, "paired"));
    let mut orphan = system.clone();
    orphan.push(finder[1].clone());
    cases.push(("orphan-finder-metadata".to_string(), *b"INIT", orphan, "paired"));
    for index in 0..2
    {
        let mut compressed = system.clone();
        let mut other = finder.clone();
        other[index].attributes |= 1;
        compressed.extend(other);
        cases.push((format!("compressed-finder-{index}"),
                    *b"INIT",
                    compressed,
                    "uncompressed IDs"));
    }
    let mut malformed = system.clone();
    let mut other = finder.clone();
    other[1].data = vec![0];
    malformed.extend(other);
    cases.push(("malformed-finder-contract".to_string(), *b"INIT", malformed, "contract metadata"));
    let mut wrong = system.clone();
    let mut other = finder.clone();
    other[1].data[60..62].copy_from_slice(&0i16.to_le_bytes());
    wrong.extend(other);
    cases.push(("wrong-finder-contract-target".to_string(),
                *b"INIT",
                wrong,
                "wrong execution target"));
    let mut driver = part(PackageKind::Driver, 0);
    driver.push(finder[1].clone());
    cases.push(("eabi1-in-driver".to_string(), *b"DRVR", driver, "another executable framing"));
    cases.push(("finder-only".to_string(), *b"INIT", finder.clone(), "Finder-only"));
    let mut files: Vec<_> =
        cases.iter().map(|(name, kind, rows, _)| (name.clone(), *kind, rows.clone())).collect();
    files.push(("good".into(), *b"INIT", system.clone()));
    let disk = Disk::new(&files);
    for (name, _, _, fragment) in cases
    {
        let (result, code_allocations, _) = disk.read(&name, Some(code_size));
        assert_eq!(code_allocations, 0,
                   "{name} tried to admit System code before rejecting complete package metadata");
        let cause = result.err().unwrap();
        assert_eq!(cause.status(), mac_abi::BAD_ABI, "{name}: {cause}");
        assert!(cause.to_string().contains(fragment) && cause.to_string().contains("rebuild"),
                "{name}: {cause}");
        assert!(matches!(cause.format(), Some(mac_formats::Error::Malformed(_))));
    }
    let (good, _, activity) = disk.read("good", None);
    let good = good.unwrap();
    assert_eq!(activity.writes, 0);
    assert_eq!(activity.flushes, 0);
    assert_eq!(mac_formats::module::Package::inspect(&good, *b"INIT").unwrap().contract.code_id, 0);
}

#[test]
fn dual_target_projection_keeps_finder_body_on_disk_and_admits_only_the_system_pair()
{
    let system = part(PackageKind::Extension, 0);
    let mut dual = system.clone();
    let mut finder = part(PackageKind::Extension, 1);
    finder[0].data = vec![0xaa; mac_module::MAX_CODE_BYTES];
    // This proves the precise boundary: Finder framing and metadata are checked,
    // but an unselected Finder body/digest is not executed or validated here.
    dual.extend(finder);
    let disk = Disk::new(&[("good".into(), *b"INIT", system), ("dual".into(), *b"INIT", dual)]);
    let (_, _, baseline) = disk.read("good", None);
    let (result, large_allocations, activity) = disk.read("dual", Some(mac_module::MAX_CODE_BYTES));
    assert_eq!(large_allocations, 0, "unselected Finder code was allocated");
    let resources = result.unwrap();
    assert_eq!(resources.iter().filter(|row| row.kind == *b"RV64").count(), 1);
    assert_eq!(resources.iter().filter(|row| row.kind == *b"EABI").count(), 1);
    assert!(resources.iter()
                     .filter(|row| [*b"RV64", *b"EABI"].contains(&row.kind))
                     .all(|row| row.id == 0));
    assert_eq!(mac_formats::module::Package::inspect(&resources, *b"INIT").unwrap()
                                                                          .contract
                                                                          .code_id,
               0);
    assert!(activity.reads <= baseline.reads + 24,
            "unselected Finder body was read: baseline={}, dual={}",
            baseline.reads,
            activity.reads);
    assert_eq!(activity.writes, 0);
}

#[test]
fn real_projection_allocation_refusal_preserves_the_package_and_retry_reads_the_same_system_part()
{
    let resources = part(PackageKind::Extension, 0);
    let code_bytes = resources[0].data.len();
    let observed = Observe::fail_at(1);
    let index = mac_formats::module::validate_index(*b"INIT", resources.iter().map(mac_formats::module::ResourceHeader::from));
    let allocations = observed.finish();
    index.unwrap();
    assert_eq!(allocations, 0, "shared complete-index validation allocated a private projection");
    let disk = Disk::new(&[("good".into(), *b"INIT", resources)]);
    let (_, activity) =
        testing::with_file_disk(&disk.path, true, DiskFaults::default(), |services| {
            // SAFETY: this scope pins the real File Manager service table.
            let system = unsafe { System::from_raw(services) }.unwrap();
            let volume = system.files().volume_at(0).unwrap().id;
            let file = system.files().lookup(volume, Cnid(2), "good").unwrap();
            // The first real HFS-B allocation occurs in the mounted volume's
            // name admission. Its cause must survive both filesystem adapters.
            let observed = Observe::fail_at(1);
            let refusal = testing::files::module_resources(volume.0, file.id.0);
            let attempts = observed.finish();
            assert!(attempts > 0);
            let cause = refusal.err().unwrap();
            assert_eq!(cause.storage(), Some(mac_abi::NO_MEMORY), "{cause}");
            assert!(cause.to_string().contains("memory") && cause.to_string().contains("retry"),
                    "{cause}");
            assert_eq!(system.files().catalog(volume, file.id).unwrap().name(), "good");
            // Refuse the real selected code-buffer reservation, after the
            // filesystem's own bounded metadata admission. The distinctive
            // payload size does not target an unrelated filesystem allocation.
            let observed = Observe::begin(true, Some(code_bytes));
            let refusal = testing::files::module_resources(volume.0, file.id.0);
            let attempts = observed.finish();
            assert!(attempts > 0);
            let cause = refusal.err().unwrap();
            assert_eq!(cause.status(), mac_abi::NO_MEMORY, "{cause}");
            assert!(matches!(cause.format(), Some(mac_formats::Error::AllocationFailed(_))));
            assert!(cause.to_string().contains("memory") && cause.to_string().contains("retry"),
                    "{cause}");
            let resources = testing::files::module_resources(volume.0, file.id.0).unwrap();
            assert_eq!(mac_formats::module::Package::inspect(&resources, *b"INIT").unwrap()
                                                                                  .contract
                                                                                  .code_id,
                       0);
        }).unwrap();
    assert_eq!((activity.writes, activity.flushes), (0, 0));
}
