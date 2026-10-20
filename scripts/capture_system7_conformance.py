#!/usr/bin/env python3
"""Transport for the aggregate Rust userspace conformance application.

Build/package a private Finder, replace it only on a cloned disk, boot the real
ROM/System graphically (headless display), and retain serial/outcome evidence.
Rust owns every compatibility assertion and the acceptance marker.
"""
from hfs_b_tool import Image as NativeImage
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import system7_vm as vm


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def private_disk(output, fixture_name='conformance'):
    fixture = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture_name
    target = vm.ROOT / 'build/system7' / (fixture_name + '-probe-target')
    manifest = fixture / 'Cargo.toml'
    subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked',
                    '--release', '--target', 'riscv64imac-unknown-none-elf',
                    '--bin', 'Finder', '--target-dir', str(target)], cwd=vm.ROOT, check=True)
    binary = target / 'riscv64imac-unknown-none-elf/release/Finder'
    packaged = output / 'package'
    subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked',
                    '--bin', 'package', '--target-dir', str(target), '--', str(binary),
                    str(packaged)], cwd=vm.ROOT, check=True)
    private = output / 'dist'
    private.mkdir()
    raw = private / 'conformance.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(vm.DIST / 'system7.qcow2'), str(raw)], check=True)

    fs = NativeImage(raw)
    fs.remove('/System Folder/Finder')
    fs.import_macbinary(packaged / 'Finder.bin', '/System Folder')
    shutil.copyfile(vm.DIST / 'bootrom.bin', private / 'bootrom.bin')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    return private, binary


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: sha(original / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = private_disk(output)
    vm.DIST = private
    date_disk = vm.ROOT / 'System7/crates/mac-toolbox/tests/data/files/dates-2026.hfs'
    process, endpoint = vm.launch(output / 'vm', extra_disks=[date_disk])
    serial = output / 'vm/serial.log'
    termination = 'timeout'
    try:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if 'TOOLBOX_USERSPACE_PASS ' in text or 'TOOLBOX_USERSPACE_FAIL ' in text:
                termination = 'guest-outcome'
                break
            if process.poll() is not None:
                termination = 'process-exited'
                break
            time.sleep(.02)
        if process.poll() is None:
            vm.qmp('screendump', {'filename': str(output / 'conformance.ppm')}, endpoint)
        report = {'transport': 'real disk boot -> System -> private public SDK Finder -> ECALL',
                  'assertion_owner': 'mac-toolbox/tests/conformance',
                  'termination': termination,
                  'original_before': before,
                  'original_after': {name: sha(original / name) for name in before},
                  'fixture_elf_sha256': sha(binary),
                  'fixture_manifest_sha256': sha(vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/conformance/resources.json'),
                  'launch': json.loads((output / 'vm/command.json').read_text()),
                  'serial': 'vm/serial.log', 'serial_sha256': sha(serial)}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
