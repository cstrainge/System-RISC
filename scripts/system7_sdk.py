#!/usr/bin/env python3
"""Prepare/build the private pinned Rust SDK; never edit the installed toolchain.

The command runs under xtask's existing cross-platform build lease. A prepared
source tree is not a compiled std sysroot, and neither is native OS acceptance.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

SOURCE = Path(__file__).resolve().parent.parent
PORT = SOURCE / "platforms" / "rust"


def capture(command: list[str], **kwargs) -> str:
    return subprocess.check_output(command, text=True, **kwargs).strip()


def digest_tree(path: Path) -> str:
    result = hashlib.sha256()
    for item in sorted(path.rglob("*")):
        if item.is_file():
            relative = item.relative_to(path).as_posix().encode()
            data = item.read_bytes()
            result.update(len(relative).to_bytes(8, "little"))
            result.update(relative)
            result.update(len(data).to_bytes(8, "little"))
            result.update(data)
    return result.hexdigest()


def write_json(path: Path, value) -> None:
    temporary = path.with_suffix(path.suffix + ".new")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def toolchain(pin: dict) -> tuple[Path, Path, Path, str, str]:
    # A local alias is acceptable only when its actual full compiler hash matches.
    # Do not install/change the user's default toolchain as a side effect.
    requested = os.environ.get("SYSTEM7_RUST_TOOLCHAIN", pin["rustup_toolchain"])
    try:
        rustc = Path(capture(["rustup", "which", "--toolchain", requested, "rustc"]))
        cargo = Path(capture(["rustup", "which", "--toolchain", requested, "cargo"]))
    except subprocess.CalledProcessError as cause:
        raise RuntimeError(
            f"SDK requires rustc {pin['rustc_commit']}; install {pin['rustup_toolchain']} "
            "with rust-src, or select an exact matching local alias with SYSTEM7_RUST_TOOLCHAIN"
        ) from cause
    version = capture([str(rustc), "-Vv"])
    fields = dict(line.split(": ", 1) for line in version.splitlines() if ": " in line)
    if fields.get("commit-hash") != pin["rustc_commit"]:
        raise RuntimeError(f"wrong SDK compiler: {version}; required commit {pin['rustc_commit']}")
    sysroot = Path(capture([str(rustc), "--print", "sysroot"]))
    cargo_version = capture([str(cargo), "-Vv"])
    cargo_fields = dict(line.split(": ", 1) for line in cargo_version.splitlines() if ": " in line)
    if cargo_fields.get("commit-hash") != pin["cargo_commit"]:
        raise RuntimeError(f"wrong SDK Cargo: {cargo_version}; required commit {pin['cargo_commit']}")
    return rustc, cargo, sysroot, fields["host"], cargo_version


def mirror_host(source: Path, target: Path) -> None:
    # A directory symlink is cheap on Unix and supported Windows installations.
    # Fall back to an actual copy when Windows does not grant symlink privilege.
    try:
        target.symlink_to(source, target_is_directory=True)
    except OSError:
        shutil.copytree(source, target)


def prepare(args) -> dict:
    build = args.build_dir.resolve()
    if build == SOURCE or SOURCE in build.parents:
        raise RuntimeError("SDK output must be outside the OS source repository")
    pin = json.loads((PORT / "sdk.json").read_text(encoding="utf-8"))
    rustc, cargo, upstream, host, cargo_version = toolchain(pin)
    source = upstream / "lib/rustlib/src/rust"
    if not (source / "library/std/Cargo.toml").is_file():
        raise RuntimeError("matching rust-src is missing; install it for the pinned SDK toolchain")
    source_digest = digest_tree(source)
    if source_digest != pin["rust_src_sha256"]:
        raise RuntimeError("installed rust-src differs from the pinned SDK source; refuse to patch it")
    target_bytes = (PORT / (pin["target"] + ".json")).read_bytes()
    target = json.loads(target_bytes)
    if target["features"] != "+" + args.llvm_profile:
        raise RuntimeError("SDK target and selected platform instruction profiles differ")
    package = tomllib.loads((SOURCE / "Cargo.toml").read_text())["workspace"]["package"]
    inputs = {
        "schema": 1,
        "compiler_commit": pin["rustc_commit"],
        "cargo": cargo_version,
        "host": host,
        "upstream_source_sha256": source_digest,
        "port_sha256": digest_tree(PORT),
        "builder_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "target_sha256": hashlib.sha256(target_bytes).hexdigest(),
        "mac_abi_sha256": digest_tree(SOURCE / "crates/mac-abi/src"),
        "mac_traps_sha256": digest_tree(SOURCE / "crates/mac-traps/src"),
        "transport_package": package,
        "workspace_manifest_sha256": hashlib.sha256((SOURCE / "Cargo.toml").read_bytes()).hexdigest(),
        "workspace_lock_sha256": hashlib.sha256((SOURCE / "Cargo.lock").read_bytes()).hexdigest(),
        "cargo_config_sha256": hashlib.sha256((SOURCE / ".cargo/config.toml").read_bytes()).hexdigest(),
        "compiler_proxy_sha256": hashlib.sha256(args.compiler_proxy.read_bytes()).hexdigest(),
    }
    key = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
    root = build / "sdk" / key
    receipt = root / "prepared.json"
    if not receipt.is_file():
        temporary = root.with_name(key + ".preparing")
        if temporary.exists():
            raise RuntimeError(f"incomplete SDK preparation at {temporary}; inspect/remove it before retrying")
        temporary.mkdir(parents=True)
        try:
            private = temporary / "sysroot"
            rustlib = private / "lib/rustlib"
            rustlib.mkdir(parents=True)
            mirror_host(upstream / "lib/rustlib" / host, rustlib / host)
            copied = rustlib / "src/rust"
            shutil.copytree(source, copied)
            for relative in pin["patches"]:
                patch = (PORT / relative).resolve()
                if PORT.resolve() not in patch.parents:
                    raise RuntimeError("SDK patch escapes the port source directory")
                subprocess.run(["git", "apply", "--check", str(patch)], cwd=copied, check=True)
                subprocess.run(["git", "apply", str(patch)], cwd=copied, check=True)
            overlay = PORT / "library"
            if overlay.is_dir():
                shutil.copytree(overlay, copied / "library", dirs_exist_ok=True)
            # Resolve the two core-only transport packages into the private Rust
            # workspace; no application facade or OS implementation is copied.
            for name in ("mac-abi", "mac-traps"):
                destination = copied / "library" / name
                shutil.copytree(SOURCE / "crates" / name / "src", destination / "src")
                private_name = name.replace("mac-", "systemrisc-std-")
                manifest = (f'[package]\nname = "{private_name}"\nversion = "{package["version"]}"\n'
                            f'edition = "{package["edition"]}"\npublish = false\n\n[dependencies]\n'
                            'core = { package = "rustc-std-workspace-core", path = "../rustc-std-workspace-core" }\n')
                if name == "mac-traps":
                    manifest += 'mac-abi = { package = "systemrisc-std-abi", path = "../mac-abi" }\n'
                (destination / "Cargo.toml").write_text(manifest, encoding="utf-8")
            shutil.copyfile(PORT / (pin["target"] + ".json"), temporary / (pin["target"] + ".json"))
            write_json(temporary / "prepared.json", {
                "inputs": inputs,
                "patched_source_sha256": digest_tree(copied),
                "state": "prepared-sources-only",
                "std_runtime_qualified": False,
            })
            temporary.rename(root)
        except BaseException:
            # This is only this invocation's unpublished private staging tree.
            shutil.rmtree(temporary)
            raise
    prepared = json.loads(receipt.read_text(encoding="utf-8"))
    if prepared["inputs"] != inputs:
        raise RuntimeError("SDK cache identity does not match its inputs")
    copied = root / "sysroot/lib/rustlib/src/rust"
    if digest_tree(copied) != prepared["patched_source_sha256"]:
        raise RuntimeError(f"private SDK sources changed at {copied}; refuse a stale cache receipt")
    if hashlib.sha256((root / (pin["target"] + ".json")).read_bytes()).hexdigest() != inputs["target_sha256"]:
        raise RuntimeError("private SDK target changed; refuse a stale cache receipt")
    result = {"root": str(root), "sysroot": str(root / "sysroot"),
              "target": str(root / (pin["target"] + ".json")),
              "rustc": str(rustc), "cargo": str(cargo), "key": key}
    write_json(build / "sdk/current.json", result)
    print(f"SDK prepared: {root}\nSource preparation is not a std build or runtime qualification.", flush=True)
    return result


def build_std(args, sdk: dict) -> None:
    # Build-std is driven by a real core-only OS transport library. It does not
    # replace any guest service or run a host-linked OS validation harness.
    env = dict(os.environ)
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
        env.pop(name, None)
    for name in list(env):
        if name.startswith("CARGO_PROFILE_"):
            env.pop(name)
    # The target JSON owns ISA/PIC. Do not inherit a host's per-target flags.
    env["CARGO_ENCODED_RUSTFLAGS"] = ""
    env.update(RUSTC=str(args.compiler_proxy.resolve()),
               SYSTEM7_SDK_COMPILER=sdk["rustc"], SYSTEM7_SDK_SYSROOT=sdk["sysroot"])
    observed = capture([str(args.compiler_proxy), "--print", "sysroot"], env=env)
    if Path(observed).resolve() != Path(sdk["sysroot"]).resolve():
        raise RuntimeError("Cargo compiler proxy does not select the private SDK source root")
    command = [sdk["cargo"], "build", "--release", "--locked", "--package", "mac-traps",
               "--target", sdk["target"], "--target-dir", str(Path(sdk["root"]) / "target"),
               "-Zbuild-std=core,alloc,std,panic_abort", "-Zbuild-std-features=compiler-builtins-mem",
               "-Zjson-target-spec"]
    # Explicit settings outrank user Cargo configuration and match the recorded
    # workspace release profile. No host environment changes SDK code generation.
    profile = tomllib.loads((SOURCE / "Cargo.toml").read_text())["profile"]["release"]
    for name in ("panic", "opt-level", "lto", "codegen-units", "debug", "strip"):
        command.extend(["--config", f"profile.release.{name}={json.dumps(profile[name])}"])
    completed = subprocess.run(command, cwd=SOURCE, env=env)
    libraries = {}
    if completed.returncode == 0:
        target_name = Path(sdk["target"]).stem
        output = Path(sdk["root"]) / "target" / target_name / "release"
        installed = Path(sdk["sysroot"]) / "lib/rustlib" / target_name / "lib"
        installed.mkdir(parents=True, exist_ok=True)
        artifacts = sorted([*output.rglob("lib*.rlib"), *output.rglob("lib*.rmeta")])
        for artifact in artifacts:
            # The ordinary transport package only drives build-std. Its regular
            # Cargo crates are not part of the standard-library sysroot.
            if artifact.name in ("libmac_traps.rlib", "libmac_traps.rmeta") or artifact.name.startswith(("libmac_traps-", "libmac_abi-")):
                continue
            digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
            destination = installed / artifact.name
            if destination.exists() and hashlib.sha256(destination.read_bytes()).hexdigest() != digest:
                raise RuntimeError(f"conflicting SDK library artifact: {destination}")
            shutil.copyfile(artifact, destination)
            libraries[destination.relative_to(Path(sdk["root"])).as_posix()] = digest
        for suffix in (".rlib", ".rmeta"):
            if len([name for name in libraries if Path(name).name.startswith("libstd-") and name.endswith(suffix)]) != 1:
                raise RuntimeError(f"SDK build did not produce exactly one std {suffix} artifact")
    write_json(Path(sdk["root"]) / "build.json", {
        "command": command, "returncode": completed.returncode,
        "compiler_source_root": observed, "std_runtime_qualified": False,
        "libraries": libraries,
    })
    completed.check_returncode()
    print("Target std library compiled. Application startup and native services are not yet qualified.")


def built_sdk(sdk: dict) -> bool:
    root = Path(sdk["root"])
    receipt = root / "build.json"
    if not receipt.is_file():
        return False
    result = json.loads(receipt.read_text())
    if result.get("returncode") != 0 or not result.get("libraries"):
        return False
    return all((root / path).is_file() and
               hashlib.sha256((root / path).read_bytes()).hexdigest() == digest
               for path, digest in result["libraries"].items())


def build_app(args, sdk: dict) -> None:
    if args.manifest_path is None:
        raise RuntimeError("sdk app requires --manifest-path Cargo.toml")
    manifest = args.manifest_path.resolve()
    cargo = tomllib.loads(manifest.read_text())
    resources = manifest.parent / "resources.json"
    resource_data = json.loads(resources.read_text())
    if resource_data.get("role") != "application" or resource_data.get("file_type") != "APPL":
        raise RuntimeError("sdk app requires an ordinary native APPL application manifest")
    binary = resource_data.get("binary_name", cargo["package"]["name"])
    identity = hashlib.sha256(str(manifest).encode()).hexdigest()
    output = args.build_dir.resolve() / "applications" / identity[:16]
    output.mkdir(parents=True, exist_ok=True)
    target = output / sdk["key"][:16]
    env = dict(os.environ)
    for name in list(env):
        if name.startswith("CARGO_PROFILE_") or name in (
                "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_BUILD_BUILD_DIR", "SYSTEM7_CARGO_ARTIFACT_DIR"):
            env.pop(name, None)
    env.update(RUSTC=str(args.compiler_proxy.resolve()),
               SYSTEM7_SDK_COMPILER=sdk["rustc"], SYSTEM7_SDK_SYSROOT=sdk["sysroot"],
               RUST_TARGET_PATH=sdk["root"], CARGO_ENCODED_RUSTFLAGS="-C\x1frelocation-model=pic")
    command = [sdk["cargo"], "build", "--release", "--locked", "--manifest-path", str(manifest),
               "--bin", binary, "--target", sdk["target"], "--target-dir", str(target),
               "--message-format=json-render-diagnostics", "-Zjson-target-spec"]
    profile = tomllib.loads((SOURCE / "Cargo.toml").read_text())["profile"]["release"]
    for name in ("panic", "opt-level", "lto", "codegen-units", "debug", "strip"):
        command.extend(["--config", f"profile.release.{name}={json.dumps(profile[name])}"])
    completed = subprocess.run(command, cwd=manifest.parent, env=env, stdout=subprocess.PIPE, text=True)
    (output / "cargo-messages.jsonl").write_text(completed.stdout, encoding="utf-8")
    binaries = []
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (message.get("reason") == "compiler-artifact" and
                message.get("target", {}).get("name") == binary and message.get("executable")):
            binaries.append(message["executable"])
    report = {"sdk_key": sdk["key"], "command": command, "returncode": completed.returncode,
              "resource_manifest": str(resources), "output": str(output / "package"),
              "cargo_manifest_sha256": hashlib.sha256(manifest.read_bytes()).hexdigest(),
              "resource_manifest_sha256": hashlib.sha256(resources.read_bytes()).hexdigest(),
              "state": "compile-failed", "native_execution_observed": False}
    if completed.returncode == 0:
        if len(binaries) != 1 or not Path(binaries[0]).is_file():
            raise RuntimeError("Cargo did not report exactly one compiled application executable")
        report.update(elf=binaries[0], state="compiled-only",
                      elf_sha256=hashlib.sha256(Path(binaries[0]).read_bytes()).hexdigest())
    write_json(args.build_dir.resolve() / "sdk/application.json", report)
    completed.check_returncode()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "build", "app"))
    parser.add_argument("--build-dir", required=True, type=Path)
    parser.add_argument("--compiler-proxy", required=True, type=Path)
    parser.add_argument("--llvm-profile", required=True)
    parser.add_argument("--manifest-path", type=Path)
    args = parser.parse_args()
    sdk = prepare(args)
    if args.action == "build":
        build_std(args, sdk)
    elif args.action == "app":
        if not built_sdk(sdk):
            build_std(args, sdk)
        build_app(args, sdk)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as cause:
        print(f"System RISC SDK: {cause}", file=sys.stderr)
        sys.exit(1)
