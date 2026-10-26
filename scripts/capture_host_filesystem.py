#!/usr/bin/env python3
"""Private native FSYS -> HDIR -> virtio -> Linux companion evidence transport.
Rust owns the acceptance assertions; no live user VM or export is used here.
"""
from system7_platform import guest_env
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import time

from hfs_b_tool import Image
import system7_vm as vm


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(output, sdk=False, restore=False, disconnect=None):
    # Feature variants share a guest binary path. Cargo's build lock ends
    # before packaging, so hold our own through both steps; tests may run in
    # parallel once each has copied its package into its private output.
    fixtures = vm.build_dir() / 'fixtures'
    fixtures.mkdir(parents=True, exist_ok=True)
    with (fixtures / 'host-filesystem-package.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        _prepare(output, sdk, restore, disconnect)


def _prepare(output, sdk=False, restore=False, disconnect=None):
    fixtures = [('host-filesystem-app', 'Finder'),
                ('host-filesystem-exit-worker', 'HostExitWorker')] if sdk else [
                    ('host-filesystem-probe', 'HostFilesystemProbe')]
    if restore:
        fixtures = [('host-filesystem-app', 'Finder')]
    features = ['--features', 'metadata-restore'] if restore else []
    if disconnect:
        features = ['--features', 'disconnect-' + disconnect]
    for fixture, binary in fixtures:
        selected_features = features if fixture == 'host-filesystem-app' else []
        target = vm.build_dir() / 'fixtures' / fixture
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin',
                        binary, '--target-dir', str(target), *selected_features], cwd=vm.ROOT, env=guest_env(), check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(target), *selected_features, '--',
                        str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                        str(output / 'packages')], cwd=vm.ROOT, check=True)
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')], cwd=vm.ROOT / 'System7', check=True)


def capture(output, sdk=False, disconnect=None):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    prepare(output, sdk, disconnect=disconnect)
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
            source.mkdir()
            data = b'Native catalog fixture\n'
            (source / 'data').write_bytes(data)
            (source / 'resources').write_bytes(b'')
            (source / 'metadata.json').write_text(json.dumps({
                'format': 'system-risc-forks', 'version': 1, 'directory': False,
                'name': 'Café', 'created_mac_microseconds': 3875171696123456,
                'modified_mac_microseconds': 3875171696654321,
                'backup_mac_microseconds': None, 'flags': 0, 'finder_info': [0]*32,
                'comment': 'Native comment: café 🦀',
                'data_length': len(data), 'resource_length': 0,
            }))
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
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(export),
                            str(output / 'packages/Host Exit Worker.bin'), '/Tools'], check=True)
            header = bytearray(128)
            header[1] = 6
            header[2:8] = b'Broken'
            header[65:73] = b'APPLHFST'
            malformed = b'invalid resource fork'
            header[87:91] = len(malformed).to_bytes(4, 'big')
            broken = output / 'Broken.bin'
            broken.write_bytes(header + malformed + bytes(-len(malformed) % 128))
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(export), str(broken), '/Tools'], check=True)
            # Reproduce host provenance, without the guest/offline importer.
            (export / 'Host Folder').mkdir()
            (export / 'Host Folder/Child.txt').write_bytes(b'host child\n')
            (export / 'Host File.txt').write_bytes(b'host data\n')
            (export / 'Snapshot.txt').write_bytes(b'before')
            shutil.copyfile(vm.ROOT / 'System7/crates/mac-sit/tests/fixtures/nested-v1.sit', export / 'Raw.sit')
        companion_args = [str(host / 'risc-host'), '--export', f'Test={export}']
        if sdk:
            readonly = output / 'readonly-export'
            subprocess.run([str(host / 'risc-forks'), 'init', str(readonly), 'Read Only Host'], check=True)
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(readonly),
                            str(base / 'Res Edit.bin')], check=True)
            subprocess.run([str(host / 'risc-forks'), 'import-macbinary', str(readonly),
                            str(output / 'packages/Host Exit Worker.bin')], check=True)
            (readonly / 'Host Folder').mkdir()
            (readonly / 'Host File.txt').write_bytes(b'host data\n')
            (readonly / 'Snapshot.txt').write_bytes(b'before')
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
            process, endpoint = vm.launch(output / 'vm', writable=sdk, host_bridge=port)
            termination = 'desktop-ready'
            fixture_progress = []
            if sdk:
                termination = 'timeout'
                began = time.monotonic()
                deadline = began + 30
                absolute_deadline = began + 120
                milestones = set()
                consumed = 0
                watch_changes = set()
                snapshot_changes = set()
                application_releases = {}
                disconnected = False
                while time.monotonic() < min(deadline, absolute_deadline):
                    text = (output / 'vm/serial.log').read_text(errors='replace')
                    complete = text.rfind('\n') + 1
                    for line in text[consumed:complete].splitlines():
                        if line.startswith(('HOST_FS_STAGE ', 'FILE_TRANSFER_STAGE ',
                                            'FILE_TRANSFER_EXIT_PASS', 'HOST_FILESYSTEM_')):
                            fixture_progress.append({'seconds': time.monotonic() - began,
                                                     'marker': line})
                        # This guards a stuck transport, not the total duration
                        # of an expanding correctness campaign. Only a new
                        # finite fixture milestone extends the stall budget;
                        # kernel logging/event storms cannot keep it alive.
                        if line.startswith(('HOST_FS_STAGE ', 'FILE_TRANSFER_STAGE ',
                                            'FILE_TRANSFER_EXIT_PASS')) and line not in milestones:
                            milestones.add(line)
                            deadline = time.monotonic() + 30
                    consumed = complete
                    if disconnect and not disconnected and 'TRANSFER_DISCONNECT_READY\n' in text:
                        companion.terminate()
                        companion.wait(timeout=10)
                        disconnected = True
                    for marker, directory in [('RW', export), ('RO', readonly)]:
                        if f'HOST_FS_SNAPSHOT_READY_{marker}\n' in text and marker not in snapshot_changes:
                            source = directory / 'Snapshot.txt'
                            source_stat = source.stat()
                            source.write_bytes(b'after!')
                            os.utime(source, ns=(source_stat.st_atime_ns, source_stat.st_mtime_ns))
                            vm.qmp('input-send-event', {'events': [
                                {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'}, 'down': down}}
                                for down in [True, False]]}, endpoint)
                            snapshot_changes.add(marker)
                        releases = text.count(f'HOST_FS_EXTERNAL_APPLICATION_RELEASE_{marker}\n')
                        if releases > application_releases.get(marker, 0):
                            vm.qmp('input-send-event', {'events': [
                                {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'}, 'down': down}}
                                for down in [True, False]]}, endpoint)
                            application_releases[marker] = releases
                        if f'HOST_FS_WATCH_READY_{marker}\n' in text and marker not in watch_changes:
                            (directory / 'Runtime Host File.txt').write_bytes(b'live')
                            watch_changes.add(marker)
                    if 'HOST_FILESYSTEM_PASS' in text or 'HOST_FILESYSTEM_FAIL' in text:
                        termination = 'guest-outcome'
                        break
                    if process.poll() is not None:
                        termination = 'process-exited'
                        break
                    time.sleep(.02)
            reboot = None
            if sdk and not disconnect and termination == 'guest-outcome' and 'HOST_FILESYSTEM_PASS' in text:
                first_pid = process.pid
                vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
                process = None
                prepare(output, sdk=True, restore=True)
                image.remove('/System Folder/Finder')
                image.import_macbinary(output / 'packages/Finder.bin', '/System Folder')
                subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2',
                                str(raw), str(dist / 'system7.qcow2')], check=True)
                process, endpoint = vm.launch(output / 'restore-vm', host_bridge=port)
                deadline = time.monotonic() + 30
                restore_termination = 'timeout'
                while time.monotonic() < deadline:
                    restored = (output / 'restore-vm/serial.log').read_text(errors='replace')
                    if 'HOST_METADATA_RESTORE_PASS' in restored or 'HOST_FILESYSTEM_FAIL' in restored:
                        restore_termination = 'guest-outcome'
                        break
                    if process.poll() is not None:
                        restore_termination = 'process-exited'
                        break
                    time.sleep(.02)
                reboot = {'termination': restore_termination, 'first_pid': first_pid,
                          'second_pid': process.pid, 'serial': str(output / 'restore-vm/serial.log')}
            command = json.loads((output / 'vm/command.json').read_text())
            (output / 'capture.json').write_text(json.dumps({
                'termination': termination, 'command': command, 'reboot': reboot,
                'fixture_progress': fixture_progress,
                'watchdog': {'inactivity_seconds': 30, 'absolute_seconds': 120,
                             'progress': 'unique fixture milestones only'},
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
        for folder in [dist, output / 'vm', output / 'restore-vm']:
            for pattern in ['*.raw', '*.qcow2', '*.bin']:
                for path in folder.glob(pattern):
                    path.unlink()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--sdk', action='store_true')
    parser.add_argument('--disconnect', choices=['source', 'destination'])
    args = parser.parse_args()
    capture(args.output.resolve(), args.sdk or bool(args.disconnect), args.disconnect)
