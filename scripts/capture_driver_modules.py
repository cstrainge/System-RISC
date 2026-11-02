#!/usr/bin/env python3
"""Private native-module transport. Rust creates malformed packages and owns
acceptance; this script compiles fixtures, imports requested files, and boots.
"""
from system7_fixture_build import guest_env as fixture_guest_env, host_env as fixture_host_env
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import time

from hfs_b_tool import Image
import system7_vm as vm


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def worker_reclamation(serial):
    """Correlate native lifecycle observations; never infer ACK from PASS/EXIT."""
    published = set(re.findall(r'^MODULE_WORKER_THREAD module=(\d+) thread=(\d+) ', serial, re.M))
    reclaimed = set(re.findall(r'^MODULE_WORKER_RECLAIMED module=(\d+) thread=(\d+)$', serial, re.M))
    return {'published': sorted(published), 'reclaimed': sorted(reclaimed),
            'unconfirmed': sorted(published - reclaimed)}


def prepare(output):
    output.mkdir(parents=True, exist_ok=True)
    for fixture, binary in [('driver-provider', 'ModuleEcho'), ('driver-client', 'ModuleClient')]:
        target = vm.build_dir() / 'fixtures' / fixture
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                        '--target-dir', str(target)], cwd=vm.ROOT, env=fixture_guest_env(target), check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(target), '--',
                        str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                        str(output / 'packages')], cwd=vm.ROOT, env=fixture_host_env(target), check=True)


def prepare_recovery(output):
    prepare(output)
    target = vm.build_dir() / 'fixtures/driver-abandon'
    manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/driver-abandon/Cargo.toml'
    for mode, features in [('ack', []), ('no-ack', ['--features', 'no-ack']),
                           ('reply', ['--features', 'reply'])]:
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf',
                        '--bin', 'ModuleAbandon', '--target-dir', str(target), *features],
                       cwd=vm.ROOT, env=fixture_guest_env(target), check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(target), '--',
                        str(target / 'riscv64imac-unknown-none-elf/release/ModuleAbandon'),
                        str(output / 'packages' / mode)], cwd=vm.ROOT, env=fixture_host_env(target), check=True)


def prepare_workers(output):
    """Existing module fixtures, independently linked with their diagnostic features."""
    output.mkdir(parents=True, exist_ok=True)
    artifacts = []
    for fixture, binary, features, mode in [
            ('driver-provider', 'ModuleEcho', 'workers', 'workers'),
            ('driver-client', 'ModuleClient', 'workers', 'workers'),
            ('driver-client', 'ModuleClient', 'workers-rollback', 'workers-rollback')]:
        target = vm.build_dir() / 'fixtures' / (fixture + '-workers')
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        command = ['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                   '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                   '--features', features, '--target-dir', str(target)]
        subprocess.run(command, cwd=vm.ROOT, env=fixture_guest_env(target), check=True)
        elf = target / 'riscv64imac-unknown-none-elf/release' / binary
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--features', features, '--target-dir', str(target), '--',
                        str(elf), str(output / 'packages' / mode)],
                       cwd=vm.ROOT, env=fixture_host_env(target), check=True)
        artifacts.append({'fixture': fixture, 'features': features, 'mode': mode,
                          'elf_sha256': digest(elf), 'command': command})
    (output / 'worker-fixtures.json').write_text(json.dumps(artifacts, indent=2) + '\n')


def prepare_pressure(output):
    """Genuine per-identity native relinks through the existing normal packager.

    No final DABI is edited and no build-time contract check is bypassed. The
    actor receives one recorded exact-image inspection grant on the private disk.
    """
    output.mkdir(parents=True, exist_ok=True)
    artifacts = []
    def build(fixture, binary, destination, *, ordinal=None, system=False):
        target = vm.build_dir() / 'fixtures' / (fixture + '-pressure')
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        native_env = fixture_guest_env(target)
        package_env = fixture_host_env(target)
        if ordinal is not None:
            declaration = {'SYSTEM7_PRESSURE_IDENTITY': f'7731a00000000000000000000000{0xaa01+ordinal:04x}',
                           'SYSTEM7_PRESSURE_NAME': f'Pressure Holder {ordinal:02d}',
                           'SYSTEM7_PRESSURE_ORDINAL': str(ordinal)}
            native_env.update(declaration)
            package_env.update(declaration)
        else:
            declaration = {}
        command = ['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                   '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                   '--features', 'pressure', '--target-dir', str(target)]
        subprocess.run(command, cwd=vm.ROOT, env=native_env, check=True)
        elf = target / 'riscv64imac-unknown-none-elf/release' / binary
        retained = destination / (binary + '.elf')
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(elf, retained)
        package_command = ['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                           '--bin', 'package', '--features', 'pressure', '--target-dir', str(target),
                           '--', str(retained), str(destination)]
        if system:
            package_command.append(str(vm.DIST / 'System.bin'))
        subprocess.run(package_command, cwd=vm.ROOT, env=package_env, check=True)
        artifacts.append({'fixture': fixture, 'features': 'pressure', 'declaration': declaration,
                          'elf': str(retained), 'elf_sha256': digest(retained),
                          'module_load_bytes': int((destination / 'module-load-bytes.txt').read_text()) if not system else None,
                          'command': command, 'package_command': package_command,
                          'packages': [{'path': str(path), 'sha256': digest(path)}
                                       for path in sorted(destination.glob('*.bin'))]})
    build('driver-provider', 'ModuleEcho', output / 'packages' / 'controller')
    for ordinal in range(44):
        build('driver-client', 'ModuleClient', output / 'packages' / f'holder-{ordinal:02d}', ordinal=ordinal)
    build('process-observer', 'Finder', output / 'packages' / 'actor', system=True)
    module_load_bytes = sum(item['module_load_bytes'] or 0 for item in artifacts)
    if module_load_bytes >= 8 * 1024 * 1024:
        raise ValueError('pressure modules alone exceed the configured physical module arena')
    files = [{'source': str(output / 'packages/controller/Pressure Control.bin'),
              'directory': '/System Folder/Drivers'}]
    files += [{'source': str(output / 'packages' / f'holder-{i:02d}' / f'Pressure Holder {i:02d}.bin'),
               'directory': '/System Folder/Drivers'} for i in range(44)]
    broker_case = {
        'name': 'broker44', 'preserve_drivers': True, 'files': list(files),
        'until': 'BROKER_CLIENT_READY ', 'timeout': 120, 'boot_timeout': 120,
        'required_markers': [f'BROKER_CLIENT_READY ordinal={i} ' for i in range(44)],
        'qualification_scope': '44 real workers: strict Start-only registration and Status round trips '
                               'completed with cleared tickets; no GUI actor or heap-exhaustion phase'}
    (output / 'broker44-cases.json').write_text(json.dumps([broker_case], indent=2) + '\n')
    files += [{'source': str(output / 'packages/actor/Pressure Probe.bin'), 'directory': '/Applications'},
              {'source': str(output / 'packages/actor/System.bin'), 'directory': '/System Folder',
               'replace': '/System Folder/System'}]
    case = {'name': 'module-ordinary-pressure', 'preserve_drivers': True, 'files': files,
            'launch_pressure_actor': {'applications': [40, 95], 'actor': [184, 106]},
            'until': 'MODULE_ORDINARY_PRESSURE_PASS ', 'timeout': 420, 'boot_timeout': 120,
            'required_markers': ['PRESSURE_GLOBAL_WITNESS ', 'PRESSURE_FULL_REPORT_PASS bytes=1048576',
                                 'PRESSURE_HOLD ', 'ordinal=43', 'minimum_refused=true',
                                 'MODULE_ORDINARY_PRESSURE_PASS ']}
    (output / 'pressure-cases.json').write_text(json.dumps([case], indent=2) + '\n')
    (output / 'pressure-fixtures.json').write_text(json.dumps({
        'artifacts': artifacts, 'worker_count': 44, 'holder_payload_bytes': [4194304, 4194304, 3145728],
        'finisher_max_payload_bytes': 8388608, 'module_load_bytes': module_load_bytes,
        'arena_limit_bytes': 8388608, 'installed_driver_bytes_not_included': True, 'clock': 'QEMU rdtime 10MHz',
        'many_runnable_workers': True, 'timing_qualified': False,
        'inspection_grant': 'private exact-image INSPECT_DOMAINS; existing grants preserved',
        'broker_case': broker_case, 'case': case}, indent=2) + '\n')


def capture(output, cases):
    original = vm.DIST
    before = {name: digest(original / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    observations = []
    for case in cases:
        name = case['name']
        if not name or not all(c.isascii() and (c.isalnum() or c == '-') for c in name):
            raise ValueError('invalid case directory')
        state = output / name
        dist = state / 'dist'
        dist.mkdir(parents=True, exist_ok=False)
        raw = dist / 'module-test.raw'
        process = None
        try:
            subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                            str(original / 'system7.qcow2'), str(raw)], check=True)
            image = Image(raw)
            if name != 'host-device-absent' and not case.get('preserve_drivers', False):
                # Malformed/synthetic module cases need an isolated discovery
                # set. The host-device case exercises the image's installation.
                # Enumerate the private installation, so every future external
                # driver is excluded from these deliberately synthetic cases.
                for line in image.run('list', '/System Folder/Drivers').splitlines():
                    entry = json.loads(line)
                    if not entry['directory']:
                        image.remove(entry['id'])
            for file in case['files']:
                if file.get('replace'):
                    image.remove(file['replace'])
                image.import_macbinary(Path(file['source']), file['directory'])
            shutil.copyfile(original / 'bootrom.bin', dist / 'bootrom.bin')
            subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                            str(dist / 'system7.qcow2')], check=True)
            vm.DIST = dist
            process, endpoint = vm.launch(state / 'vm', boot_timeout=case.get('boot_timeout'))
            termination = 'desktop-ready'
            mouse = None
            input_before_marker = None
            if case.get('launch_pressure_actor'):
                from capture_system7_applications_ui import Driver
                driver = Driver(endpoint, state / 'vm/serial.log')
                launch = case['launch_pressure_actor']
                driver.click(*launch['applications'], double=True)
                driver.wait_folder_ready('Applications')
                driver.click(*launch['actor'], double=True)
            if case.get('input_during'):
                from capture_system7_applications_ui import Driver
                driver = Driver(endpoint, state / 'vm/serial.log')
                driver.move(115, 96)
                mouse = driver.state()['mouse']
                input_before_marker = case['until'] not in (state / 'vm/serial.log').read_text(errors='replace')
            if case.get('until'):
                termination = 'timeout'
                deadline = time.monotonic() + case.get('timeout', 10)
                while time.monotonic() < deadline:
                    serial = (state / 'vm/serial.log').read_text(errors='replace')
                    if any(marker in serial for marker in ['PRESSURE_ACTOR_FAIL', 'PRESSURE_WORKER_FAIL',
                                                           'PRESSURE_EXCHANGE_FAIL', 'SYSTEM_PANIC:',
                                                           'SYSTEM_TRAP:', 'MODULE_PANIC']):
                        termination = 'guest-failure'
                        break
                    if case['until'] in serial and all(
                            marker in serial for marker in case.get('required_markers', [])):
                        lifetimes = worker_reclamation(serial) if case.get('wait_worker_reclamation') else None
                        if lifetimes is None or (lifetimes['published'] and not lifetimes['unconfirmed']):
                            termination = 'requested-marker'
                            break
                    if process.poll() is not None:
                        termination = 'process-exited'
                        break
                    time.sleep(.01)
            if case.get('input'):
                from capture_system7_applications_ui import Driver
                driver = Driver(endpoint, state / 'vm/serial.log')
                driver.move(115, 96)
                mouse = driver.state()['mouse']
            serial_text = (state / 'vm/serial.log').read_text(errors='replace')
            required = case.get('required_markers', [])
            if termination == 'requested-marker' and not all(marker in serial_text for marker in required):
                termination = 'missing-required-markers'
            observations.append({'name': name, 'required_markers': {marker: marker in serial_text for marker in required},
                                 'qualification_scope': case.get('qualification_scope'),
                                 'worker_reclamation': worker_reclamation(serial_text)
                                                       if case.get('wait_worker_reclamation') else None,
                                 'launch_pressure_actor': case.get('launch_pressure_actor'),
                                 'preserved_installed_drivers': case.get('preserve_drivers', False), 'termination': termination, 'mouse': mouse,
                                 'serial': str(state / 'vm/serial.log'),
                                 'input_completed_before_marker': input_before_marker})
        finally:
            if process is not None:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
            # Failed launches also clean their private media; the base and live
            # user desktop are never altered by this transport.
            for folder in [dist, state / 'vm']:
                for pattern in ['*.raw', '*.qcow2', '*.bin']:
                    for path in folder.glob(pattern):
                        path.unlink()
            vm.DIST = original
    report = {'cases': observations, 'original_before': before,
              'original_after': {name: digest(original / name) for name in before}}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--prepare', action='store_true')
    parser.add_argument('--prepare-recovery', action='store_true')
    parser.add_argument('--prepare-workers', action='store_true')
    parser.add_argument('--prepare-pressure', action='store_true')
    parser.add_argument('--cases', type=Path)
    args = parser.parse_args()
    if args.prepare:
        prepare(args.output.resolve())
    if args.prepare_recovery:
        prepare_recovery(args.output.resolve())
    if args.prepare_workers:
        prepare_workers(args.output.resolve())
    if args.prepare_pressure:
        prepare_pressure(args.output.resolve())
    if args.cases:
        capture(args.output.resolve(), json.loads(args.cases.read_text()))
