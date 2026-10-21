#!/usr/bin/env python3
"""Private native FSYS -> HDIR -> virtio -> Linux companion evidence transport.
Rust owns the acceptance assertions; no live user VM or export is used here.
"""
import argparse
import hashlib
import json
from pathlib import Path
import select
import shutil
import subprocess
import time

from hfs_b_tool import Image
import system7_vm as vm


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(output, sdk=False):
    fixture = 'host-filesystem-app' if sdk else 'host-filesystem-probe'
    binary = 'Finder' if sdk else 'HostFilesystemProbe'
    target = vm.build_dir() / 'fixtures' / fixture
    manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
    subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                    '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin',
                    binary, '--target-dir', str(target)], cwd=vm.ROOT, check=True)
    subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                    '--bin', 'package', '--target-dir', str(target), '--',
                    str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                    str(output / 'packages')], cwd=vm.ROOT, check=True)
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')], cwd=vm.ROOT / 'System7', check=True)


def capture(output, sdk=False):
    output.mkdir(parents=True, exist_ok=False)
    prepare(output, sdk)
    base = vm.DIST
    before = {name: digest(base / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    dist = output / 'dist'
    dist.mkdir()
    raw = dist / 'native-host.raw'
    companion = process = None
    endpoint = None
    try:
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(base / 'system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        for name in ['Host Transport', 'Host Filesystem']:
            image.import_macbinary(base / f'{name}.bin', '/System Folder/Drivers')
        if sdk:
            image.remove('/System Folder/Finder')
            image.import_macbinary(output / 'packages/Finder.bin', '/System Folder')
        else:
            image.import_macbinary(output / 'packages/Host Filesystem Probe.bin', '/System Folder/Extensions')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                        str(dist / 'system7.qcow2')], check=True)
        shutil.copyfile(base / 'bootrom.bin', dist / 'bootrom.bin')
        export = output / 'export'
        host = vm.build_dir() / 'host/debug'
        subprocess.run([str(host / 'risc-forks'), 'init', str(export), 'Native Host'], check=True)
        companion_args = [str(host / 'risc-host'), '--export', f'Test={export}']
        if sdk:
            readonly = output / 'readonly-export'
            subprocess.run([str(host / 'risc-forks'), 'init', str(readonly), 'Read Only Host'], check=True)
            companion_args.extend(['--read-only-export', f'ReadOnly={readonly}'])
        with (output / 'companion.log').open('wb') as log:
            companion = subprocess.Popen(companion_args,
                                         stdout=subprocess.PIPE, stderr=log)
            if not select.select([companion.stdout], [], [], 10)[0]:
                raise TimeoutError('private host companion did not start')
            ready = companion.stdout.readline().decode()
            if not ready.startswith('HOST_BRIDGE_READY 127.0.0.1:'):
                raise RuntimeError('invalid companion readiness: ' + ready)
            (output / 'companion-ready.txt').write_text(ready)
            port = int(ready.rsplit(':', 1)[1])
            vm.DIST = dist
            process, endpoint = vm.launch(output / 'vm', host_bridge=port)
            termination = 'desktop-ready'
            if sdk:
                termination = 'timeout'
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    text = (output / 'vm/serial.log').read_text(errors='replace')
                    if 'HOST_FILESYSTEM_PASS' in text or 'HOST_FILESYSTEM_FAIL' in text:
                        termination = 'guest-outcome'
                        break
                    if process.poll() is not None:
                        termination = 'process-exited'
                        break
                    time.sleep(.02)
            command = json.loads((output / 'vm/command.json').read_text())
            (output / 'capture.json').write_text(json.dumps({
                'termination': termination, 'command': command,
                'serial': str(output / 'vm/serial.log'),
                'original_before': before,
                'original_after': {name: digest(base / name) for name in before}
            }, indent=2) + '\n')
    finally:
        if process is not None:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
        if companion is not None:
            companion.terminate()
            companion.wait(timeout=10)
            companion.stdout.close()
        vm.DIST = base
        # Keep evidence, current images and original media. Delete private disks
        # even when admission/boot/protocol assertions fail.
        for folder in [dist, output / 'vm']:
            for pattern in ['*.raw', '*.qcow2', '*.bin']:
                for path in folder.glob(pattern):
                    path.unlink()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--sdk', action='store_true')
    args = parser.parse_args()
    capture(args.output.resolve(), args.sdk)
