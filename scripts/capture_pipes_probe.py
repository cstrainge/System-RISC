#!/usr/bin/env python3
"""Transport for the guest pipe probe.

Builds the probe fixture with the public SDK, installs it as Finder on a cloned
image, boots the real ROM and System, and retains the serial log. Assertions live
in the fixture; Python only moves artifacts and records the command.
"""
from hfs_b_tool import Image as NativeImage
from system7_fixture_build import guest_env as fixture_guest_env, host_env as fixture_host_env
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import system7_vm as vm

QEMU = os.environ.get('SYSTEM7_WINDOWS_QEMU', r'C:\Program Files\qemu\qemu-system-riscv64.exe')
FIXTURE = 'pipes-probe'
MARKERS = ['PIPES_PROBE_PASS', 'PIPES_PROBE_FAIL']


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def build(output):
    manifest = vm.SOURCE / 'crates/mac-toolbox/tests/fixtures' / FIXTURE / 'Cargo.toml'
    target = vm.build_dir() / 'fixtures' / FIXTURE
    subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked',
                    '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', 'Finder',
                    '--target-dir', str(target)], cwd=vm.ROOT,
                   env=fixture_guest_env(target), check=True)
    elf = target / 'riscv64imac-unknown-none-elf/release' / 'Finder'
    package = output / 'package'
    subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked',
                    '--bin', 'package', '--target-dir', str(target), '--', str(elf),
                    str(package)], cwd=vm.ROOT, env=fixture_host_env(target), check=True)
    return elf, package / 'Finder.bin'


def probe_disk(output, package):
    raw = output / 'probe.hfsb'
    shutil.copyfile(vm.DIST / 'system7.hfsb', raw)
    fs = NativeImage(raw)
    fs.remove('/System Folder/Finder')
    fs.import_macbinary(package, '/System Folder')
    return raw


def command(disk, bootrom):
    return [QEMU, '-machine', 'virt', '-cpu', 'rva23s64,mmu=on,pmp=on,sv39=on',
            '-m', '2G', '-smp', '4', '-bios', 'none',
            '-drive', f'if=pflash,unit=0,format=raw,readonly=on,file={bootrom}',
            '-global', 'virtio-mmio.force-legacy=false', '-monitor', 'none', '-nic', 'none',
            '-device', 'virtio-gpu-device,bus=virtio-mmio-bus.6', '-device', 'ramfb',
            '-device', 'virtio-keyboard-device,bus=virtio-mmio-bus.1',
            '-device', 'virtio-tablet-device,bus=virtio-mmio-bus.2',
            '-object', 'rng-builtin,id=system-rng',
            '-device', 'virtio-rng-device,rng=system-rng,bus=virtio-mmio-bus.5',
            '-display', 'none', '-serial', 'stdio',
            '-drive', f'if=none,id=disk0,readonly=on,format=raw,file={disk}',
            '-device', 'virtio-blk-device,drive=disk0,bus=virtio-mmio-bus.0']


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {'status': 'incomplete'}
    elf, package = build(output)
    disk = probe_disk(output, package)
    bootrom = vm.DIST / 'bootrom-riscv.bin'
    argv = command(disk, bootrom)
    serial = output / 'serial.log'
    log = serial.open('wb')
    process = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT)
    termination = 'timeout'
    deadline = time.monotonic() + 120
    try:
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if any(marker in text for marker in MARKERS) or 'SYSTEM_PANIC:' in text:
                termination = 'guest-outcome'
                break
            if process.poll() is not None:
                termination = 'process-exited'
                break
            time.sleep(.02)
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=15)
        log.close()
    text = serial.read_text(errors='replace')
    report = {'transport': 'real ROM -> System -> probe Finder on a cloned disk',
              'fixture_elf_sha256': sha(elf),
              'fixture_package_sha256': sha(package),
              'command': argv,
              'termination': termination,
              'serial': str(serial),
              'serial_sha256': sha(serial),
              'pass': 'PIPES_PROBE_PASS' in text,
              'fail': 'PIPES_PROBE_FAIL' in text,
              'markers': [line for line in text.splitlines()
                          if line.startswith('PIPES_PROBE_')],
              'finder_launched': 'PROCESS_REGISTERED app=4294967392 name=Finder' in text}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if report['pass'] and not report['fail'] else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    sys.exit(capture(parser.parse_args().output))
