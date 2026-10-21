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
    fixtures = [('host-filesystem-app', 'Finder'),
                ('host-filesystem-exit-worker', 'HostExitWorker')] if sdk else [
                    ('host-filesystem-probe', 'HostFilesystemProbe')]
    for fixture, binary in fixtures:
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
        if sdk:
            image.remove('/System Folder/Finder')
            image.import_macbinary(output / 'packages/Finder.bin', '/System Folder')
            image.import_macbinary(output / 'packages/Host Exit Worker.bin', '/Applications')
        else:
            image.import_macbinary(output / 'packages/Host Filesystem Probe.bin', '/System Folder/Extensions')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                        str(dist / 'system7.qcow2')], check=True)
        shutil.copyfile(base / 'bootrom.bin', dist / 'bootrom.bin')
        export = output / 'export'
        host = vm.build_dir() / 'host/debug'
        subprocess.run([str(host / 'risc-forks'), 'init', str(export), 'Native Host'], check=True)
        if sdk:
            source = output / 'Café'
            source.write_bytes(b'Native catalog fixture\n')
            subprocess.run([str(host / 'risc-forks'), 'import', str(export), str(source)], check=True)
            subprocess.run([str(host / 'risc-forks'), 'mkdir', str(export), '/', 'Tools'], check=True)
            child = output / 'Child'
            child.write_bytes(b'Nested fixture\n')
            subprocess.run([str(host / 'risc-forks'), 'import', str(export), str(child), '/Tools'], check=True)
            # Original MacBinary envelope is fixture transport, not an export
            # acceptance oracle. Rust verifies both payloads through File APIs.
            header = bytearray(128)
            header[1] = 5
            header[2:7] = b'Forks'
            header[65:73] = b'TEXTTEST'
            data = bytes((i * 31 + 7) & 255 for i in range(131089))
            resources = bytes((i * 13 + 11) & 255 for i in range(70003))
            header[83:87] = len(data).to_bytes(4, 'big')
            header[87:91] = len(resources).to_bytes(4, 'big')
            forks = output / 'Forks.bin'
            forks.write_bytes(header + data + bytes(-len(data) % 128)
                              + resources + bytes(-len(resources) % 128))
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(export), str(forks), '/Tools'], check=True)
            # Reproduce host provenance, without the guest/offline importer.
            (export / 'Host Folder').mkdir()
            (export / 'Host Folder/Child.txt').write_bytes(b'host child\n')
            (export / 'Host File.txt').write_bytes(b'host data\n')
            shutil.copyfile(vm.ROOT / 'System7/crates/mac-sit/tests/fixtures/nested-v1.sit', export / 'Raw.sit')
        companion_args = [str(host / 'risc-host'), '--export', f'Test={export}']
        if sdk:
            readonly = output / 'readonly-export'
            subprocess.run([str(host / 'risc-forks'), 'init', str(readonly), 'Read Only Host'], check=True)
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(readonly),
                            str(base / 'ResEdit.bin')], check=True)
            (readonly / 'Host Folder').mkdir()
            (readonly / 'Host File.txt').write_bytes(b'host data\n')
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
