mod disk;
mod smoke;
mod boot;

use std::{env,
          ffi::OsStr,
          fs,
          path::{Path, PathBuf},
          process::{Command, ExitCode}};

use mac_build::{Result, error, package};
use serde_json::{Value, json};

const TARGET: &str = "riscv64imac-unknown-none-elf";

struct Workspace
{
    root:  PathBuf,
    repo:  PathBuf,
    build: PathBuf
}

impl Workspace
{
    fn new(build: Option<PathBuf>, checking: bool) -> Result<Self>
    {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
        let repo = root.parent().unwrap().to_path_buf();
        let build = match build
        {
            Some(path) => env::current_dir()?.join(path),
            None =>
            {
                let normal = mac_build::paths::build_dir(&repo);
                if checking { normal.join("check") } else { normal }
            }
        };
        fs::create_dir_all(&build)?;
        let build = build.canonicalize()?;
        if build.starts_with(&root)
        {
            return Err(error("build output must be outside the OS source repository"));
        }
        Ok(Self { root,
                  repo,
                  build })
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command
    {
        let mut command = Command::new(program);
        command.env(mac_build::paths::BUILD_DIR_ENV, &self.build);
        command
    }

    fn describe(&self, path: &Path) -> String
    {
        path.strip_prefix(&self.repo).unwrap_or(path).display().to_string()
    }

    fn cargo(&self) -> Command
    {
        let mut command = self.command(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command.current_dir(&self.root);
        command
    }

    fn guest_build(&self, packages: &[&str]) -> Result<()>
    {
        let mut command = self.cargo();
        command.args(["build", "--locked", "--release", "--target", TARGET, "--target-dir"])
               .arg(self.build.join("target"));
        for package in packages
        {
            command.args(["--package", package]);
        }
        for package in packages
        {
            if matches!(*package,
                        "mac-system"
                        | "native-wdef"
                        | "native-cdef"
                        | "native-scrollbar-cdef"
                        | "native-mdef")
            {
                command.arg("--features").arg(format!("{package}/guest"));
            }
        }
        if packages.contains(&"mac-system") && env::var_os("SYSTEM7_EVENT_TRACE").is_some()
        {
            command.args(["--features", "mac-system/event-trace"]);
        }
        run(&mut command)
    }

    fn build(&self) -> Result<Vec<Value>>
    {
        self.guest_build(&["native-wdef", "native-cdef", "native-scrollbar-cdef", "native-mdef"])?;
        self.guest_build(&["mac-system", "mac-finder", "quickdraw-test", "textedit-exerciser",
                           "cursor-test", "panic-test", "resedit", "bootrom"])?;
        boot::package_rom(self)?;
        let mut reports = Vec::new();
        for (directory, name, binary) in
            [("system", "System", "System"),
             ("finder", "Finder", "Finder"),
             ("apps/quickdraw-test", "QuickDraw Test", "QuickDrawTest"),
             ("apps/textedit-exerciser", "TextEdit Exerciser", "TextEditExerciser"),
             ("apps/cursor-test", "Cursor Test", "CursorTest"),
             ("apps/panic-test", "Panic Test", "PanicTest"),
             ("apps/resedit", "ResEdit", "ResEdit")]
        {
            let report = package::build(&self.root.join(directory).join("resources.json"),
                                        &self.build
                                             .join("target")
                                             .join(TARGET)
                                             .join("release")
                                             .join(binary),
                                        &self.build.join("dist"))?;
            println!("Packaged {name}: entry 0x{:x}, {} resources",
                     report["entry"].as_u64().unwrap(),
                     report["resources"].as_array().unwrap().len());
            reports.push(report);
        }
        for (index, a) in reports.iter().enumerate()
        {
            for b in &reports[index + 1..]
            {
                if a["load_base"].as_u64() < b["load_limit"].as_u64()
                   && b["load_base"].as_u64() < a["load_limit"].as_u64()
                {
                    return Err(error("native executable memory reservations overlap"));
                }
            }
        }
        let report = json!({"target": TARGET, "guest_profile": "release", "build_dir": self.build, "toolchain": {"cargo": output(self.cargo().arg("--version"))?, "rustc": output(Command::new("rustc").arg("--version"))?}, "packages": reports});
        write_json(&self.build.join("dist/build.json"), &report)?;
        Ok(reports)
    }

    fn private_guest_test(&self, name: &str) -> Result<()>
    {
        let checked = run(self.cargo().args(["test", "--locked", "-p", "mac-toolbox", "--test",
                                             name, "--", "--ignored"]));
        let cleaned = run(self.command("python3")
                              .current_dir(&self.repo)
                              .arg("System7/scripts/prune_system7_images.py"));
        checked?;
        cleaned?;
        Ok(())
    }

    fn check(&self) -> Result<()>
    {
        run(self.command("python3")
                .current_dir(&self.root)
                .args(["-m", "unittest", "discover", "-s", "scripts/tests", "-p", "test_*.py"]))?;
        run(self.command("python3")
                .current_dir(&self.repo)
                .arg("System7/scripts/prune_system7_images.py"))?;
        run(self.cargo().args(["fmt", "--all", "--", "--check"]))?;
        run(self.cargo().args(["test", "--locked", "--workspace", "--exclude", "mac-finder",
                               "--exclude", "quickdraw-test", "--exclude",
                               "textedit-exerciser", "--exclude", "cursor-test", "--exclude",
                               "panic-test", "--exclude", "resedit", "--exclude", "abi-smoke",
                               "--exclude", "bootrom", "--exclude", "native-wdef", "--exclude",
                               "native-cdef", "--exclude", "native-mdef"]))?;
        // The large-fork fixture writes every logical block through the shared
        // core, then verifies 64-bit File Manager transport through the SDK.
        run(self.cargo().args(["test", "--locked", "--release", "-p", "mac-toolbox", "--test",
                               "hfs_b", "allocated_native_fork", "--", "--ignored"]))?;
        let reports = self.build()?;
        run(self.command("python3")
                .current_dir(&self.repo)
                .arg("System7/scripts/check_system7_boundaries.py"))?;
        let boundaries: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             toolbox-boundaries.json"))?)?;
        let image = disk::image(self)?;
        let smoke = smoke::check(self, &reports)?;
        for test in ["userspace_graphical", "window_interactions", "finder_png"]
        {
            self.private_guest_test(test)?;
        }
        for test in ["dates", "boot_ui", "resedit", "menus", "finder_trash", "file_writes_guest",
                     "deferred_close", "get_info_ui", "events", "clipboard", "textedit_graphical",
                     "typing", "processes", "application_ui", "retained_update", "event_latency",
                     "cursor_graphical", "busy_cursor", "panic_graphical"]
        {
            self.private_guest_test(test)?;
        }
        let resource_desktop: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             finder-trash-checks.json"))?)?;
        let writable: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             filesystem-write-checks.json"))?)?;
        let userspace: Value =
            serde_json::from_slice(&fs::read(self.repo
                                                 .join("results/system7/\
                                                        userspace-conformance-checks.json"))?)?;
        let boot = boot::check(self)?;
        run(self.command("python3")
                .current_dir(&self.repo)
                .args(["System7/scripts/system7_vm.py", "check", "--no-build", "--build-dir"])
                .arg(&self.build))?;
        let desktop: Value = serde_json::from_slice(&fs::read(self.repo
                                                                  .join("results/system7/\
                                                                         desktop-checks.json"))?)?;
        run(self.command("python3")
                .current_dir(&self.repo)
                .arg("System7/scripts/check_wdef_pixels.py"))?;
        let appearance: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             wdef-appearance-checks.json"))?)?;
        let mut quickdraw = serde_json::Map::new();
        for (name, script, report) in [("primitives",
                                        "System7/scripts/check_quickdraw_primitives.py",
                                        "results/quickdraw-reference/primitive-checks.json"),
                                       ("fonts",
                                        "System7/scripts/check_quickdraw_fonts.py",
                                        "results/system7/font-comparison-checks.json"),
                                       ("icons",
                                        "System7/scripts/check_quickdraw_icons.py",
                                        "results/quickdraw-reference/icon-checks.json"),
                                       ("resources",
                                        "System7/scripts/check_system7_resources.py",
                                        "results/system7/resource-manager-checks.json")]
        {
            run(self.command("python3").current_dir(&self.repo).arg(script))?;
            quickdraw.insert(name.into(),
                             serde_json::from_slice(&fs::read(self.repo.join(report))?)?);
        }
        let mut finder = serde_json::Map::new();
        for (name, script, report) in [("icons",
                                        "System7/scripts/check_finder_icons.py",
                                        "results/system7/finder-icon-checks.json"),
                                       ("scrollbars",
                                        "System7/scripts/check_scrollbar_pixels.py",
                                        "results/system7/scrollbar-appearance-checks.json"),
                                       ("mounted_volumes",
                                        "System7/scripts/check_finder_volumes.py",
                                        "results/system7/finder-volume-checks.json")]
        {
            run(self.command("python3").current_dir(&self.repo).arg(script))?;
            finder.insert(name.into(), serde_json::from_slice(&fs::read(self.repo.join(report))?)?);
        }
        run(self.command("python3")
                .current_dir(&self.repo)
                .args(["System7/scripts/check_system7_files.py", "--skip-clippy"]))?;
        let filesystem: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             filesystem-checks.json"))?)?;
        let event_manager: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             event-manager-checks.json"))?)?;
        let clipboard: Value =
            serde_json::from_slice(&fs::read(self.repo
                                                 .join("results/system7/clipboard-checks.json"))?)?;
        let textedit: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             textedit-graphical-checks.json"))?)?;
        let typing: Value = serde_json::from_slice(&fs::read(self.repo
                                                                 .join("results/system7/\
                                                                        typing-checks.json"))?)?;
        let processes: Value =
            serde_json::from_slice(&fs::read(self.repo
                                                 .join("results/system7/process-checks.json"))?)?;
        let application_ui: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             application-ui-checks.json"))?)?;
        let retained_update: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             retained-update-checks.json"))?)?;
        let event_latency: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             event-latency-checks.json"))?)?;
        let cursors: Value = serde_json::from_slice(&fs::read(self.repo
                                                                  .join("results/system7/\
                                                                         cursor-checks.json"))?)?;
        let panics: Value = serde_json::from_slice(&fs::read(self.repo
                                                                 .join("results/system7/\
                                                                        panic-checks.json"))?)?;
        let report = json!({
            "target": TARGET, "host_tests": "passed", "packages": reports, "toolbox_boundaries": boundaries, "disk": image, "execution": smoke, "graphical_userspace":userspace,"event_manager":event_manager,"clipboard":clipboard,"textedit":textedit,"typing":typing,"processes":processes,"application_ui":application_ui,"retained_update":retained_update,"event_latency":event_latency,"cursors":cursors,"panics":panics,"boot": boot, "desktop": desktop, "wdef_appearance": appearance, "quickdraw": quickdraw, "filesystem":filesystem,"writable_filesystem":writable,"resource_desktop":resource_desktop,"finder":finder,
            "scope": "Native resource-fork System/Finder, flash ROM boot from blessed HFS-B, resource-loaded definitions, resident QuickDraw/fonts/icons/pictures, resource menus/alerts, captured Event/Scrap and monochrome TextEdit services with Finder exercisers, shared native HFS-B read/write services with UTF-8 names and microsecond dates, unchanged classic HFS exchange access, and read-only resource-fork inspection. Original visual comparisons are strict for primitives/icons/bitmap fonts; TrueType and synthetic styles retain measured pixel differences. Original 68k execution and full classic Toolbox remain incomplete."
        });
        let path = self.repo.join("results/system7/toolchain-checks.json");
        write_json(&path, &report)?;
        println!("System/Finder toolchain checks passed: {}", path.display());
        Ok(())
    }
}

fn run(command: &mut Command) -> Result<()>
{
    let status = command.status()?;
    if !status.success()
    {
        return Err(error(format!("command failed ({status}): {command:?}")));
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<String>
{
    let output = command.output()?;
    if !output.status.success()
    {
        return Err(error(format!("command failed: {command:?}\n{}",
                                 String::from_utf8_lossy(&output.stderr))));
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn write_json(path: &Path, value: &Value) -> Result<()>
{
    fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn main() -> ExitCode
{
    match execute()
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) =>
        {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<()>
{
    let mut args = Vec::new();
    let mut build = None;
    let mut input = env::args().skip(1);
    while let Some(arg) = input.next()
    {
        if arg == "--build-dir"
        {
            if build.is_some()
            {
                return Err(error("--build-dir may only be supplied once"));
            }
            build =
                Some(PathBuf::from(input.next()
                                        .ok_or_else(|| error("--build-dir needs a directory"))?));
        }
        else
        {
            args.push(arg);
        }
    }
    let checking =
        args.first().is_some_and(|s| matches!(s.as_str(), "check" | "test" | "boot-check"));
    let workspace = Workspace::new(build, checking)?;
    let lease = if args.first().is_some_and(|s| {
                                   matches!(s.as_str(),
                                            "build"
                                            | "image"
                                            | "rom"
                                            | "boot"
                                            | "boot-check"
                                            | "check"
                                            | "test")
                               })
    {
        Some(BuildLease::acquire(&workspace.build)?)
    }
    else
    {
        None
    };
    match args.as_slice()
    {
        [command] if command == "build" =>
        {
            workspace.build()?;
        }
        [command] if command == "image" =>
        {
            workspace.build()?;
            disk::image(&workspace)?;
        }
        [command] if command == "check" || command == "test" =>
        {
            let checked = workspace.check();
            drop(lease);
            // A failed compatibility check must not strand its writable disk
            // copies. Preserve the check's error after cleanup has run.
            let cleaned = run(workspace.command("python3")
                                       .current_dir(&workspace.repo)
                                       .arg("System7/scripts/prune_system7_images.py"));
            checked?;
            cleaned?;
        }
        [command] if command == "rom" =>
        {
            workspace.guest_build(&["bootrom"])?;
            boot::package_rom(&workspace)?;
        }
        [command] if command == "boot-check" =>
        {
            workspace.build()?;
            disk::image(&workspace)?;
            boot::check(&workspace)?;
        }
        [command] if command == "boot" =>
        {
            workspace.build()?;
            disk::image(&workspace)?;
            boot::interactive(&workspace, &workspace.build.join("dist/system7.qcow2"), "qcow2")?;
        }
        [command, path] if command == "boot" =>
        {
            workspace.guest_build(&["bootrom"])?;
            boot::package_rom(&workspace)?;
            let path = Path::new(path).canonicalize()?;
            let format = boot::disk_format(&path)?;
            boot::interactive(&workspace, &path, format)?;
        }
        [command, path] if command == "inspect" =>
        {
            println!("{}", serde_json::to_string_pretty(&package::inspect(&fs::read(path)?)?)?)
        }
        [command, manifest, directory] if command == "rebuild" =>
        {
            println!("{}",
                     serde_json::to_string_pretty(&package::rebuild(Path::new(manifest),
                                                                    Path::new(directory))?)?);
        }
        [command, manifest, executable, directory] if command == "package" =>
        {
            let report =
                package::build(Path::new(manifest), Path::new(executable), Path::new(directory))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        [] => help(),
        [command] if command == "help" || command == "--help" => help(),
        _ => return Err(error("unknown command; use cargo xtask help"))
    }
    Ok(())
}

fn help()
{
    println!("System7 build tools\n\n  cargo xtask rebuild MANIFEST OUT  Rebuild a ResEdit \
              resource archive\n  cargo xtask build          Build System, Finder, applications \
              and boot ROM\n  cargo xtask image          Build a separate 500 MiB HFS-B disk and \
              qcow2\n  cargo xtask rom            Build the read-only flash ROM\n  cargo xtask \
              boot [DISK]    Boot from a read-only raw/qcow2 disk; serial console\n  cargo xtask \
              boot-check     Verify ROM disk boot and rejection cases\n  cargo xtask inspect \
              FILE   Validate a native MacBinary file\n  cargo xtask package MANIFEST ELF OUT  \
              Package a native application\n  cargo xtask check          Host, disk, ABI, ROM \
              boot and graphical desktop checks");
    println!("\n  --build-dir DIRECTORY     Override all build/media output (also \
              SYSTEM7_BUILD_DIR)\n  check defaults to build/system7/check; normal builds use \
              build/system7");
}

/// Keep cleanup away from active output and reject two writers to the same
/// profile. The OS lock releases on crashes; a subsequent owner replaces pid.
struct BuildLease
{
    _lock: fs::File,
    pid:   PathBuf
}
impl BuildLease
{
    fn acquire(directory: &Path) -> Result<Self>
    {
        let lock = fs::OpenOptions::new().read(true)
                                         .write(true)
                                         .create(true)
                                         .truncate(false)
                                         .open(directory.join(".build.lock"))?;
        lock.try_lock()
            .map_err(|e| {
                error(format!("build directory {} is already in use: {e}", directory.display()))
            })?;
        let pid = directory.join("pid");
        fs::write(&pid, format!("{}\n", std::process::id()))?;
        Ok(Self { _lock: lock,
                  pid })
    }
}
impl Drop for BuildLease
{
    fn drop(&mut self)
    {
        let _ = fs::remove_file(&self.pid);
    }
}
