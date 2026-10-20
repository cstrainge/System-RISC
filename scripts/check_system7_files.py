#!/usr/bin/env python3
"""Verify resident filesystem reads against unchanged native and original media."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
from system7_paths import dist_dir, describe
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-clippy", action="store_true")
    parser.add_argument("--original", type=Path, default=ROOT / "working/quickdraw-reference/reference.raw")
    parser.add_argument("--original-system", type=Path, default=ROOT / "working/quickdraw-reference/reference-System.bin")
    args = parser.parse_args()
    output = ROOT / "results/system7/files"
    output.mkdir(parents=True, exist_ok=True)
    report = {"status": "running", "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "read_only": True, "host_input_injected": False, "checks": [], "disk_reads": [],
              "limits": {"devices": 32, "volumes": 128, "open_forks": 64, "path_utf8_bytes": 4096},
              "limitations": ["This script checks reads; write/rollback acceptance runs in the SDK tests",
                              "HFS-B v1 and classic HFS; no HFS+", "512-byte sectors/APM only",
                              "No hotplug/unmount; extents-overflow tree itself must fit inline extents",
                              "Native paths use slash syntax; slash-containing names require catalog/CNID operations",
                              "Seek restricted to [0, fork_length]"]}
    target = ROOT / "build/system7/files-work"
    base = ["cargo", "test", "--manifest-path", "System7/Cargo.toml", "--target-dir", str(target)]

    def run(name, command, env=None):
        result = subprocess.run(command, cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = output / (name + ".log")
        log.write_text(result.stdout)
        report["checks"].append({"name": name, "command": command, "status": "passed" if result.returncode == 0 else "failed",
                                 "exit_code": result.returncode, "log": str(log.relative_to(ROOT))})
        if result.returncode:
            raise RuntimeError(f"{name} failed: {log}")

    def disk_case(name, disk, fork):
        before = digest(disk)
        expected = digest(fork)
        env = dict(os.environ, SYSTEM7_TEST_HFS=str(disk), SYSTEM7_TEST_SYSTEM_RSRC=str(fork))
        run(name, base + ["-p", "mac-system", "--lib", "toolbox::files::tests::actual_disk_catalog_and_both_forks",
                          "--", "--ignored", "--nocapture"], env)
        if before != digest(disk) or expected != digest(fork):
            raise RuntimeError("Disk or expected fork changed while being checked")
        report["disk_reads"].append({"name": name, "disk": str(disk.relative_to(ROOT)) if disk.is_relative_to(ROOT) else str(disk),
                                     "disk_sha256": before, "disk_unchanged": True,
                                     "expected_system_resource_fork": describe(fork),
                                     "system_resource_fork_sha256": expected, "system_resource_fork_bytes": fork.stat().st_size,
                                     "both_forks_streamed": True, "resident_resource_fork_exact": True})

    try:
        run("hfs-fixtures", base + ["-p", "mac-hfs"])
        run("resident-and-sdk", base + ["-p", "mac-system", "--lib", "toolbox::files", "--", "--nocapture"])
        disk_case("generated-native-hfs-b", dist_dir() / "system7.hfsb", dist_dir() / "System.rsrc")
        if args.original.exists() and args.original_system.exists():
            blob = args.original_system.read_bytes()
            data_size, size = struct.unpack_from(">II", blob, 83)
            start = 128 + (data_size + 127) // 128 * 128
            if start + size > len(blob):
                raise ValueError("Truncated original MacBinary fork")
            fork = ROOT / "working/filesystem-checks/original-System.rsrc"
            fork.parent.mkdir(parents=True, exist_ok=True)
            fork.write_bytes(blob[start:start + size])
            disk_case("original-system-7.6.1-hfs", args.original.resolve(), fork)
            report["original_macbinary_sha256"] = digest(args.original_system)
        else:
            report["original_reference"] = "unavailable; original runtime disk check skipped"
        if not args.skip_clippy:
            common = ["cargo", "clippy", "--manifest-path", "System7/Cargo.toml"]
            run("host-clippy", common + ["--target-dir", str(target), "-p", "mac-hfs", "-p", "toolbox-files", "-p", "mac-system", "--lib", "--tests", "--", "-D", "warnings"])
            run("rv64-clippy", common + ["--target-dir", str(ROOT / "build/system7/files-rv64"), "--target", "riscv64imac-unknown-none-elf", "-p", "mac-system", "-p", "toolbox-files", "--lib", "--", "-D", "warnings"])
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error)
    destination = ROOT / "results/system7/filesystem-checks.json"
    destination.write_text(json.dumps(report, indent=2) + "\n")
    print(f"File Manager checks {report['status']}: {destination.relative_to(ROOT)}")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
