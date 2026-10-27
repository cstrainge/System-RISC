#!/usr/bin/env python3
"""Private real-INIT boot screenshots. Rust owns SDK/pixel acceptance.

GDB stops the unchanged optimized System at its ELF discovery entry and return.
No guest delay, production instrumentation, host clipboard or normal VM monitor
is involved. Only an explicitly selected private installation is read.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from hfs_b_tool import Image
from riscv_vm import qmp
from system7_capture import OwnedProcesses
from system7_paths import BUILD_DIR_ENV, SOURCE, build_dir
from system7_platform import CPU, RAM, guest_env

CASES = [('artwork', 'Startup Artwork Probe'),
         ('fallback', 'Startup Fallback Probe'),
         ('refusal', 'Startup Refusal Probe')]
DISCOVERY = 'mac_system::drivers::runtime::discovery::discover'


def run(arguments, **options):
    return subprocess.run(arguments, cwd=SOURCE, check=True, **options)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    for case, _ in CASES:
        manifest = SOURCE / 'crates/mac-toolbox/tests/fixtures/extension-startup' / case / 'Cargo.toml'
        target = output / 'build' / case
        run(['cargo', '+nightly', 'build', '--locked', '--release', '--manifest-path', str(manifest),
             '--target', 'riscv64imac-unknown-none-elf', '--target-dir', str(target),
             '--bin', 'StartupStripProbe'], env=guest_env())
        run(['cargo', '+nightly', 'run', '--locked', '--manifest-path', str(manifest),
             '--target-dir', str(output / 'build/host'), '--bin', 'package', '--',
             str(target / 'riscv64imac-unknown-none-elf/release/StartupStripProbe'),
             str(output / 'packages')])


def discovery_entry(elf):
    symbols = run(['nm', '-C', str(elf)], capture_output=True, text=True).stdout
    matches = [line.split()[0] for line in symbols.splitlines()
               if line.endswith(' ' + DISCOVERY)]
    if len(matches) != 1:
        raise RuntimeError('optimized System ELF must contain exactly one module discovery '
                           'entry; rebuild the private System with debug symbols')
    return int(matches[0], 16)


def wait_until(predicate, description, owners, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        for owner in owners:
            if owner['process'].poll() is not None:
                raise RuntimeError(f"owned {owner['kind']} exited before {description}; "
                                   'inspect its retained log')
        time.sleep(.02)
    raise TimeoutError(f'timed out waiting for {description}; inspect retained GDB/QEMU/serial logs')


def capture(output):
    if not os.environ.get(BUILD_DIR_ENV):
        raise RuntimeError('set SYSTEM7_BUILD_DIR to an explicitly built private optimized installation')
    output = Path(output).resolve()
    original = build_dir() / 'dist'
    names = ['system7.qcow2', 'bootrom.bin', 'System.elf', 'System.rsrc']
    before = {name: digest(original / name) for name in names}
    prepare(output)
    state = output / 'vm'
    state.mkdir(exist_ok=False)
    dist = output / 'dist'
    dist.mkdir(exist_ok=False)
    media = [dist / 'startup.raw', state / 'system7.qcow2', state / 'bootrom.bin']
    owners, endpoint, errors, failure = [], None, [], None
    owned = OwnedProcesses()
    cleanup_failure = None
    finalization_failures = []
    sockets = Path(tempfile.mkdtemp(prefix='srisc-init-'))
    report = {'scope': 'native-startup-strip-only', 'host_clipboard_peer': False,
              'original_before': before, 'cases': [case for case, _ in CASES],
              'cleanup_errors': errors}
    old_build = os.environ[BUILD_DIR_ENV]
    try:
        # Even the host image adapter's compiler cache belongs to this campaign.
        os.environ[BUILD_DIR_ENV] = str(output / 'build/tools')
        run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
             str(original / 'system7.qcow2'), str(media[0])])
        image = Image(media[0])
        for line in image.run('list', '/System Folder/Extensions').splitlines():
            entry = json.loads(line)
            if entry['directory']:
                raise RuntimeError('private startup fixture requires a flat Extensions directory')
            image.remove(entry['id'])
        for _, name in CASES:
            image.import_macbinary(output / 'packages' / (name + '.bin'), '/System Folder/Extensions')
        run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(media[0]), str(media[1])])
        shutil.copyfile(original / 'bootrom.bin', media[2])
        shutil.copyfile(original / 'System.rsrc', output / 'System.rsrc')
        entry = discovery_entry(original / 'System.elf')
        report['discovery_symbol'] = DISCOVERY
        report['discovery_entry'] = hex(entry)
        endpoint = {'transport': 'unix', 'path': str(sockets / 'qmp')}
        serial = state / 'serial.log'
        args = ['qemu-system-riscv64', '-machine', 'virt', '-cpu', CPU, '-m', RAM,
                '-smp', '1', '-bios', 'none', '-drive',
                f'if=pflash,unit=0,format=raw,readonly=on,file={media[2]}', '-drive',
                f'if=none,id=startup,format=qcow2,readonly=on,file={media[1]}',
                '-global', 'virtio-mmio.force-legacy=false', '-device',
                'virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0', '-device', 'ramfb',
                '-device', 'virtio-keyboard-device,bus=virtio-mmio-bus.1', '-device',
                'virtio-tablet-device,bus=virtio-mmio-bus.2', '-display', 'none', '-nic', 'none',
                '-serial', f'file:{serial}', '-parallel', 'none', '-monitor', 'none',
                '-qmp', f'unix:{sockets}/qmp,server=on,wait=off',
                '-gdb', f'unix:{sockets}/gdb,server=on,wait=off', '-S']
        report['command'] = args
        (state / 'command.json').write_text(json.dumps(args, indent=2) + '\n')
        (state / 'qmp-endpoint.json').write_text(json.dumps(endpoint) + '\n')
        # Pauses occur entirely in the debugger. The optimized guest is unchanged.
        commands = ['set pagination off', 'set confirm off',
                    'file ' + json.dumps(str(original / 'System.elf')),
                    f'target remote {sockets}/gdb', f'hbreak *{hex(entry)}', 'continue']
        for phase in ['before', 'after']:
            if phase == 'after':
                commands.append('finish')
            commands += ['python', 'import gdb, pathlib, time',
                         f'pathlib.Path({str(output / (phase + ".ready"))!r}).write_text(hex(int(gdb.parse_and_eval("$pc"))))',
                         f'while not pathlib.Path({str(output / (phase + ".release"))!r}).exists():',
                         '    time.sleep(.02)', 'end']
        commands += ['detach', 'quit']
        gdb_commands = output / 'commands.gdb'
        gdb_commands.write_text('\n'.join(commands) + '\n')
        with (state / 'qemu.log').open('wb') as log:
            child = owned.add(subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=log, stderr=log),
                              endpoint)
        owners.append({'kind': 'qemu', 'process': child, 'reaped': False, 'exit_code': None})
        (state / 'pid').write_text(str(child.pid) + '\n')
        wait_until(lambda: (sockets / 'gdb').exists(), 'private GDB socket', owners, 10)
        with (output / 'gdb.log').open('wb') as log:
            debugger = owned.add(subprocess.Popen(['gdb-multiarch', '-batch', '-x', str(gdb_commands)],
                                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log))
        owners.append({'kind': 'gdb', 'process': debugger, 'reaped': False, 'exit_code': None})
        for phase in ['before', 'after']:
            ready = output / (phase + '.ready')
            wait_until(ready.is_file, phase + ' discovery breakpoint', owners)
            qmp('screendump', {'filename': str(output / (phase + '.ppm'))}, endpoint)
            (output / (phase + '.serial.log')).write_text(serial.read_text(errors='replace'))
            report[phase + '_pc'] = ready.read_text()
            (output / (phase + '.release')).touch()
        debugger.wait(timeout=10)
        if debugger.returncode != 0:
            raise RuntimeError('private GDB capture failed; inspect gdb.log')
        wait_until(lambda: serial.exists() and 'DESKTOP_READY' in serial.read_text(errors='replace'),
                   'usable desktop after the refused INIT', owners[:1])
        from capture_system7_applications_ui import Driver
        driver = Driver(endpoint, serial)
        driver.move(115, 96)
        report['mouse'] = driver.state()['mouse']
        qmp('screendump', {'filename': str(output / 'desktop.ppm')}, endpoint)
        report['termination'] = 'desktop-ready'
    except BaseException as error:
        failure = error
        report['capture_error'] = f'{type(error).__name__}: {error}'
    finally:
        os.environ[BUILD_DIR_ENV] = old_build
        try:
            cleanup = owned.cleanup(output, [dist, state])
        except BaseException as error:
            cleanup_failure = error
            path = output / 'cleanup.json'
            cleanup = json.loads(path.read_text()) if path.exists() else {
                'owners': [], 'errors': [str(error)], 'quarantined': True,
                'media': [str(path) for path in media if path.exists()]}
        errors.extend(cleanup['errors'])
        report['owners'] = cleanup['owners']
        report['original_after'] = {name: digest(original / name) for name in names}
        if not cleanup['quarantined']:
            try:
                shutil.rmtree(sockets)
            except OSError as error:
                finalization_failures.append(error)
                errors.append(f'private socket cleanup: {error}')
        else:
            report['media_quarantine'] = cleanup['media']
        try:
            (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
        except OSError as error:
            finalization_failures.append(error)
    if failure is not None or cleanup_failure is not None or finalization_failures:
        raise BaseExceptionGroup('private startup-strip capture failed',
                                 ([failure] if failure is not None else [])
                                 + ([cleanup_failure] if cleanup_failure is not None else [])
                                 + finalization_failures)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--prepare-only', action='store_true')
    args = parser.parse_args()
    if args.prepare_only:
        prepare(args.output)
    else:
        capture(args.output)
