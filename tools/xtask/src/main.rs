mod disk;
mod smoke;
mod boot;

use std::{env, fs,
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
    fn new() -> Self
    {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
        let repo = root.parent().unwrap().to_path_buf();
        let build = repo.join("build/system7");
        Self { root,
               repo,
               build }
    }

    fn cargo(&self) -> Command
    {
        let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
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
        run(&mut command)
    }

    fn build(&self) -> Result<Vec<Value>>
    {
        self.guest_build(&["native-wdef", "native-cdef", "native-scrollbar-cdef", "native-mdef"])?;
        self.guest_build(&["mac-system", "mac-finder", "bootrom"])?;
        boot::package_rom(self)?;
        let mut reports = Vec::new();
        for (directory, name) in [("system", "System"), ("finder", "Finder")]
        {
            let report =
                package::build(&self.root.join(directory).join("resources.json"),
                               &self.build.join("target").join(TARGET).join("release").join(name),
                               &self.build.join("dist"))?;
            println!("Packaged {name}: entry 0x{:x}, {} resources",
                     report["entry"].as_u64().unwrap(),
                     report["resources"].as_array().unwrap().len());
            reports.push(report);
        }
        let a = (&reports[0]["load_base"], &reports[0]["load_limit"]);
        let b = (&reports[1]["load_base"], &reports[1]["load_limit"]);
        if a.0.as_u64() < b.1.as_u64() && b.0.as_u64() < a.1.as_u64()
        {
            return Err(error("System and Finder memory reservations overlap"));
        }
        let report = json!({"target": TARGET, "toolchain": {"cargo": output(self.cargo().arg("--version"))?, "rustc": output(Command::new("rustc").arg("--version"))?}, "packages": reports});
        write_json(&self.build.join("dist/build.json"), &report)?;
        Ok(reports)
    }

    fn check(&self) -> Result<()>
    {
        run(self.cargo().args(["fmt", "--all", "--", "--check"]))?;
        run(self.cargo()
                .args(["test", "--locked", "--workspace", "--exclude", "mac-finder", "--exclude",
                       "abi-smoke", "--exclude", "bootrom", "--exclude", "native-wdef",
                       "--exclude", "native-cdef", "--exclude", "native-mdef"]))?;
        let reports = self.build()?;
        run(Command::new("python3").current_dir(&self.repo)
                                   .arg("System7/scripts/check_system7_boundaries.py"))?;
        let boundaries: Value =
            serde_json::from_slice(&fs::read(self.repo.join("results/system7/\
                                                             toolbox-boundaries.json"))?)?;
        let image = disk::image(self)?;
        let smoke = smoke::check(self, &reports)?;
        run(self.cargo().args(["test", "--locked", "-p", "mac-toolbox", "--test",
                               "userspace_graphical", "--", "--ignored"]))?;
        run(self.cargo().args(["test", "--locked", "-p", "mac-toolbox", "--test",
                               "window_interactions", "--", "--ignored"]))?;
        run(self.cargo().args(["test", "--locked", "-p", "mac-toolbox", "--test", "finder_png",
                               "--", "--ignored"]))?;
        for test in ["menus", "finder_trash", "file_writes_guest", "events", "clipboard",
                     "textedit_graphical"]
        {
            run(self.cargo().args(["test", "--locked", "-p", "mac-toolbox", "--test", test,
                                   "--", "--ignored"]))?;
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
        run(Command::new("python3").current_dir(&self.repo)
                                   .args(["System7/scripts/system7_vm.py", "check",
                                          "--no-build"]))?;
        let desktop: Value = serde_json::from_slice(&fs::read(self.repo
                                                                  .join("results/system7/\
                                                                         desktop-checks.json"))?)?;
        run(Command::new("python3").current_dir(&self.repo)
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
            run(Command::new("python3").current_dir(&self.repo).arg(script))?;
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
            run(Command::new("python3").current_dir(&self.repo).arg(script))?;
            finder.insert(name.into(), serde_json::from_slice(&fs::read(self.repo.join(report))?)?);
        }
        run(Command::new("python3").current_dir(&self.repo)
                                   .args(["System7/scripts/check_system7_files.py",
                                          "--skip-clippy"]))?;
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
        let report = json!({
            "target": TARGET, "host_tests": "passed", "packages": reports, "toolbox_boundaries": boundaries, "disk": image, "execution": smoke, "graphical_userspace":userspace,"event_manager":event_manager,"clipboard":clipboard,"textedit":textedit,"boot": boot, "desktop": desktop, "wdef_appearance": appearance, "quickdraw": quickdraw, "filesystem":filesystem,"writable_filesystem":writable,"resource_desktop":resource_desktop,"finder":finder,
            "scope": "Native resource-fork System/Finder, flash ROM boot from blessed HFS, resource-loaded definitions, resident QuickDraw/fonts/icons/pictures, resource menus/alerts, captured Event/Scrap and monochrome TextEdit services with Finder exercisers, bounded durable HFS catalog/Trash writes and read-only Resource Manager. Original visual comparisons are strict for primitives/icons/bitmap fonts; TrueType and synthetic styles retain measured pixel differences. Original 68k execution and full classic Toolbox remain incomplete."
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
    let workspace = Workspace::new();
    let args: Vec<String> = env::args().skip(1).collect();
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
        [command] if command == "check" || command == "test" => workspace.check()?,
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
        [] => help(),
        [command] if command == "help" || command == "--help" => help(),
        _ => return Err(error("unknown command; use cargo xtask help"))
    }
    Ok(())
}

fn help()
{
    println!("System7 build tools\n\n  cargo xtask build          Build System, Finder and boot \
              ROM\n  cargo xtask image          Build a separate 500 MiB HFS disk and qcow2\n  \
              cargo xtask rom            Build the read-only flash ROM\n  cargo xtask boot \
              [DISK]    Boot from a read-only raw/qcow2 disk; serial console\n  cargo xtask \
              boot-check     Verify ROM disk boot and rejection cases\n  cargo xtask inspect \
              FILE   Validate a native MacBinary file\n  cargo xtask check          Host, disk, \
              ABI, ROM boot and graphical desktop checks");
}
