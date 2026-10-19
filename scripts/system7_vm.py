#!/usr/bin/env python3
"""Build and boot the System7 disk desktop through its read-only flash ROM."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from riscv_vm import native_executable, qmp as send_qmp, windows_path

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / 'working/system7-vm'
DIST = ROOT / 'build/system7/dist'


def qmp(command, arguments=None, endpoint=None):
    if endpoint is None:
        endpoint = json.loads((STATE / 'qmp-endpoint.json').read_text())
    return send_qmp(command, arguments, endpoint)


def build():
    subprocess.run(['cargo', 'xtask', 'image'], cwd=ROOT / 'System7', check=True)


def launch(state, frontend='headless', extra_disks=()):
    """Launch an isolated read-only disk snapshot; never use -kernel or a loader."""
    state = Path(state).resolve()
    state.mkdir(parents=True, exist_ok=True)
    native = frontend == 'native'
    if native and not native_executable():
        raise FileNotFoundError('Portable qemu-system-riscv64.exe missing; use --frontend gtk')
    for name in ['bootrom.bin', 'system7.qcow2']:
        shutil.copyfile(DIST / name, state / name)
    executable = str(native_executable()) if native else 'qemu-system-riscv64'
    host_path = windows_path if native else str
    endpoint = ({'transport': 'tcp', 'host': '127.0.0.1', 'port': 4447, 'platform': 'windows'}
                if native else {'transport': 'unix', 'path': str(state / 'qmp.sock')})
    socket_path = state / 'qmp.sock'
    if not native and socket_path.exists():
        socket_path.unlink()
    monitor = ('tcp:127.0.0.1:4447,server=on,wait=off' if native
               else f'unix:{socket_path},server=on,wait=off')
    # GTK keeps host absolute input and the 2x guest viewport in the same space.
    display = {'native': 'gtk,zoom-to-fit=off,scale=2,show-menubar=off',
               'gtk': 'gtk,zoom-to-fit=off,scale=2', 'headless': 'none'}[frontend]
    serial = state / 'serial.log'
    serial.write_text('')
    args = [executable, '-name', 'RISCy System - Finder', '-machine', 'virt', '-cpu', 'rv64',
            '-m', '128M', '-smp', '1', '-bios', 'none',
            '-drive', 'if=pflash,unit=0,format=raw,readonly=on,file=' + host_path(state / 'bootrom.bin'),
            '-drive', 'if=none,id=startup,format=qcow2,readonly=on,file=' + host_path(state / 'system7.qcow2'),
            '-global', 'virtio-mmio.force-legacy=false',
            '-device', 'virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0',
            '-device', 'ramfb', '-device', 'virtio-keyboard-device', '-device', 'virtio-tablet-device',
            '-display', display, '-nic', 'none', '-serial', 'file:' + host_path(serial),
            '-parallel', 'none', '-monitor', 'none', '-qmp', monitor]
    for index, source in enumerate(extra_disks, start=1):
        if index > 4:
            raise ValueError('At most four additional disks fit beside graphics/input devices')
        source = Path(source).resolve()
        destination = state / f'extra-{index}.raw'
        shutil.copyfile(source, destination)
        args.extend(['-drive',f'if=none,id=extra{index},format=raw,readonly=on,file='+host_path(destination),
                     '-device',f'virtio-blk-device,drive=extra{index},bus=virtio-mmio-bus.{index+3}'])
    env = dict(os.environ, GDK_BACKEND='x11')
    if native:
        settings = {'GDK_BACKEND': 'win32', 'GDK_SCALE': '1'}
        env.update(settings)
        env['WSLENV'] = ':'.join(filter(None, [env.get('WSLENV', ''), *settings]))
    (state / 'command.json').write_text(json.dumps(args, indent=2) + '\n')
    (state / 'qmp-endpoint.json').write_text(json.dumps(endpoint, indent=2) + '\n')
    with (state / 'qemu.log').open('wb') as log:
        process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   env=env, start_new_session=True)
    (state / 'pid').write_text(str(process.pid) + '\n')
    deadline = time.monotonic() + 25
    try:
        while time.monotonic() < deadline:
            log = serial.read_text(errors='replace')
            if process.poll() is not None:
                raise RuntimeError((state / 'qemu.log').read_text() + '\n' + log)
            if 'DESKTOP_READY' in log:
                qmp('query-status', endpoint=endpoint)
                return process, endpoint
            if any(marker in log for marker in ['BOOT_NO_SYSTEM:', 'BOOT_TRAP:', 'BOOT_PANIC:',
                                               'SYSTEM_PANIC:', 'SYSTEM_TRAP:']):
                raise RuntimeError('RISCy System startup failed:\n' + log)
            time.sleep(.05)
        raise TimeoutError('Desktop did not finish boot:\n' + serial.read_text(errors='replace')
                           + '\n' + (state / 'qemu.log').read_text())
    except BaseException:
        try:
            qmp('quit', endpoint=endpoint)
        except (OSError, ValueError, RuntimeError):
            process.terminate()
        process.wait(timeout=10)
        raise


def start(frontend='auto', rebuild=True):
    try:
        status = qmp('query-status')
    except (OSError, ValueError, RuntimeError):
        pass
    else:
        print(json.dumps({'already_running': status}))
        return
    if rebuild:
        build()
    if frontend == 'auto':
        frontend = 'native' if native_executable() else 'gtk'
    process, _ = launch(STATE, frontend)
    print(json.dumps({'pid': process.pid, 'frontend': 'native-gtk' if frontend == 'native' else frontend,
                      'bootrom': str((STATE / 'bootrom.bin').relative_to(ROOT)),
                      'disk': str((STATE / 'system7.qcow2').relative_to(ROOT))}))
    print((STATE / 'serial.log').read_text())


def check(rebuild=True):
    """Cold disk boot and real virtio input in an independent headless guest."""
    from check_system7_desktop import check_events
    if rebuild:
        build()
    results = ROOT / 'results/system7'
    results.mkdir(parents=True, exist_ok=True)
    report_path = results / 'desktop-checks.json'
    report_path.write_text(json.dumps({'status': 'running'}, indent=2) + '\n')
    before = hashlib.sha256((DIST / 'system7.qcow2').read_bytes()).hexdigest()
    state = results / 'desktop-vm'
    try:
        process, endpoint = launch(state)
    except BaseException as error:
        report_path.write_text(json.dumps({'status': 'failed', 'stage': 'cold boot',
                                          'error': str(error)}, indent=2) + '\n')
        raise
    try:
        log = (state / 'serial.log').read_text(errors='replace')
        required = ['SYSTEM_FINDER_LAUNCH', 'DESKTOP_READY', 'TRAPS_READY:', 'TRAP_NESTED: definition -> OS service']
        for marker in required:
            assert marker in log, f'missing actual disk startup marker: {marker}'
        checks = check_events(endpoint, state / 'serial.log', results)
        after = hashlib.sha256((DIST / 'system7.qcow2').read_bytes()).hexdigest()
        assert before == after, 'generated disk changed during desktop checks'
        report = {'status': 'passed', 'boot': 'cold reset through read-only pflash and blessed HFS disk',
                  'finder': 'separate executable loaded by System', 'checks': checks,
                  'qemu': subprocess.check_output(['qemu-system-riscv64', '--version'], text=True).splitlines()[0],
                  'disk_unchanged': True, 'disk_sha256': before,
                  'rom_sha256': hashlib.sha256((DIST / 'bootrom.bin').read_bytes()).hexdigest(),
                  'serial': str((state / 'serial.log').relative_to(ROOT)),
                  'command': json.loads((state / 'command.json').read_text())}
        report_path.write_text(json.dumps(report, indent=2) + '\n')
        print(f'PASS: flash ROM -> blessed HFS System -> Finder; {len(checks)} desktop checks')
    except BaseException as error:
        report_path.write_text(json.dumps({'status': 'failed', 'stage': 'desktop checks',
                                          'error': str(error),
                                          'details': 'results/system7/desktop-input-checks.json'}, indent=2) + '\n')
        raise
    finally:
        try:
            qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('build')
    launch_parser = sub.add_parser('start')
    launch_parser.add_argument('--frontend', choices=['auto', 'native', 'gtk', 'headless'], default='auto')
    launch_parser.add_argument('--no-build', action='store_true')
    check_parser = sub.add_parser('check')
    check_parser.add_argument('--no-build', action='store_true')
    sub.add_parser('status')
    sub.add_parser('stop')
    shot = sub.add_parser('screenshot')
    shot.add_argument('path')
    args = parser.parse_args()
    if args.action == 'build':
        build()
    elif args.action == 'start':
        start(args.frontend, not args.no_build)
    elif args.action == 'check':
        check(not args.no_build)
    elif args.action == 'status':
        print(json.dumps(qmp('query-status')))
    elif args.action == 'stop':
        print(json.dumps(qmp('quit')))
    elif args.action == 'screenshot':
        path = Path(args.path).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        print(json.dumps(qmp('screendump', {'filename': str(path), 'format': 'png'})))


if __name__ == '__main__':
    main()
