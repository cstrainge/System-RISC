#!/usr/bin/env python3
"""Private definition fault transport. Rust owns shutdown/diagnostic acceptance."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from hfs_b_tool import Image
from system7_platform import guest_env
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    original = vm.DIST
    before = {name: packaging.sha(original / name) for name in ['System.bin', 'System.rsrc', 'system7.qcow2', 'bootrom.bin']}
    fixture = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/definition-panic'
    target = vm.build_dir() / 'fixtures/definition-panic'
    manifest = fixture / 'Cargo.toml'
    subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked', '--release',
                    '--features', 'guest', '--target', 'riscv64imac-unknown-none-elf',
                    '--bin', 'CDEF_PANIC', '--target-dir', str(target)], env=guest_env(), check=True)
    elf = target / 'riscv64imac-unknown-none-elf/release/CDEF_PANIC'
    packaged = output / 'System.bin'
    subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked', '--bin', 'package',
                    '--target-dir', str(target), '--', str(elf), str(original / 'System.bin'), str(packaged)], check=True)
    private = output / 'dist'
    private.mkdir()
    raw = private / 'definition-panic.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k', str(original / 'system7.qcow2'), str(raw)], check=True)
    image = Image(raw)
    image.remove('/System Folder/System')
    image.import_macbinary(packaged, '/System Folder')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw), str(private / 'system7.qcow2')], check=True)
    shutil.copyfile(original / 'bootrom.bin', private / 'bootrom.bin')
    process = endpoint = None
    shutdown = False
    try:
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm', frontend='headless')
        driver = Driver(endpoint, output / 'vm/serial.log')
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        try:
            driver.application('Reactor Room')
        except (OSError, RuntimeError):
            # The faulting definition may stop the guest before QMP returns.
            pass
        try:
            process.wait(timeout=10)
            shutdown = True
        except subprocess.TimeoutExpired:
            pass
        report = {'shutdown_observed': shutdown, 'exit_code': process.poll(),
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'probe_elf_sha256': packaging.sha(elf)}
    finally:
        if process is not None and process.poll() is None:
            vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
        vm.DIST = original
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    capture(parser.parse_args().output.resolve())
