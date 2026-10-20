#!/usr/bin/env python3
"""Build and boot the System7 disk desktop through its read-only flash ROM and private working disk."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import uuid

from riscv_vm import native_executable, qmp as send_qmp, windows_path
from system7_paths import BUILD_DIR_ENV, build_dir, dist_dir, describe

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / 'working/system7-vm'
DIST = dist_dir()


def qmp(command, arguments=None, endpoint=None):
    if endpoint is None:
        endpoint = json.loads((STATE / 'qmp-endpoint.json').read_text())
    return send_qmp(command, arguments, endpoint)


def build():
    subprocess.run(['cargo', 'xtask', 'image', '--build-dir', str(build_dir())],
                   cwd=ROOT / 'System7', check=True)


def launch(state, frontend='headless', extra_disks=(), *, writable=False, preserve=False,
           qmp_port=4447):
    """Launch a private disk copy. Tests default to read-only and fresh copies.

    Persistent desktop starts retain their writable copy while the build is unchanged.
    A new OS build replaces that copy; obsolete boot disks are not retained.
    """
    state = Path(state).resolve()
    state.mkdir(parents=True, exist_ok=True)
    native = frontend in ('native', 'native-headless')
    if native and not native_executable():
        raise FileNotFoundError('Portable qemu-system-riscv64.exe missing; use --frontend gtk')
    if native:
        try:
            connection = socket.create_connection(('127.0.0.1', qmp_port), timeout=.5)
        except (ConnectionRefusedError, TimeoutError):
            pass
        else:
            connection.close()
            raise RuntimeError(f'QMP port {qmp_port} is already in use; choose --qmp-port for this VM')
    shutil.copyfile(DIST / 'bootrom.bin', state / 'bootrom.bin')
    disk = state / 'system7.qcow2'
    base_hash = hashlib.sha256((DIST / 'system7.qcow2').read_bytes()).hexdigest()
    stamp = state / 'base-disk.sha256'
    same_base = stamp.exists() and stamp.read_text().strip() == base_hash
    if not preserve or not disk.exists() or not same_base:
        shutil.copyfile(DIST / 'system7.qcow2', disk)
        stamp.write_text(base_hash + '\n')
    executable = str(native_executable()) if native else 'qemu-system-riscv64'
    host_path = windows_path if native else str
    endpoint = ({'transport': 'tcp', 'host': '127.0.0.1', 'port': qmp_port, 'platform': 'windows'}
                if native else {'transport': 'unix', 'path': str(state / 'qmp.sock')})
    socket_path = state / 'qmp.sock'
    if not native and socket_path.exists():
        socket_path.unlink()
    monitor = (f'tcp:127.0.0.1:{qmp_port},server=on,wait=off' if native
               else f'unix:{socket_path},server=on,wait=off')
    # GTK keeps host absolute input and the 2x guest viewport in the same space.
    display = {'native': 'gtk,zoom-to-fit=off,scale=2,show-menubar=off',
               'gtk': 'gtk,zoom-to-fit=off,scale=2', 'headless': 'none',
               'native-headless': 'none'}[frontend]
    serial = state / 'serial.log'
    serial.write_text('')
    identity = str(uuid.uuid4())
    args = [executable, '-uuid', identity, '-name', 'System RISC - Finder', '-machine', 'virt', '-cpu', 'rv64',
            '-m', '128M', '-smp', '1', '-bios', 'none',
            '-drive', 'if=pflash,unit=0,format=raw,readonly=on,file=' + host_path(state / 'bootrom.bin'),
            '-drive', 'if=none,id=startup,format=qcow2,readonly=' + ('off' if writable else 'on') + ',file=' + host_path(disk),
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
                raise RuntimeError('System RISC startup failed:\n' + log)
            time.sleep(.05)
        raise TimeoutError('Desktop did not finish boot:\n' + serial.read_text(errors='replace')
                           + '\n' + (state / 'qemu.log').read_text())
    except BaseException:
        try:
            # A failed launch (including a competing port bind) must never quit
            # a different VM that happens to answer at this endpoint.
            if process.poll() is None and qmp('query-uuid', endpoint=endpoint)['UUID'] == identity:
                qmp('quit', endpoint=endpoint)
        except (OSError, ValueError, RuntimeError):
            pass
        if process.poll() is None:
            process.terminate()
        process.wait(timeout=10)
        raise


def start(frontend='auto', rebuild=True, qmp_port=4447):
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
    process, _ = launch(STATE, frontend, writable=True, preserve=True, qmp_port=qmp_port)
    print(json.dumps({'pid': process.pid, 'frontend': 'native-gtk' if frontend == 'native' else frontend,
                      'bootrom': describe(STATE / 'bootrom.bin'),
                      'disk': describe(STATE / 'system7.qcow2'), 'build_dir': str(build_dir())}))
    print((STATE / 'serial.log').read_text())


def check(rebuild=True, state=None):
    """Cold disk boot and real virtio input in an independent headless guest."""
    from check_system7_desktop import check_events
    if rebuild:
        build()
    results = ROOT / 'results/system7'
    results.mkdir(parents=True, exist_ok=True)
    report_path = results / 'desktop-checks.json'
    report_path.write_text(json.dumps({'status': 'running'}, indent=2) + '\n')
    before = hashlib.sha256((DIST / 'system7.qcow2').read_bytes()).hexdigest()
    state = state or build_dir() / f'desktop-vm-{os.getpid()}'
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
        report = {'status': 'passed', 'boot': 'cold reset through read-only pflash and blessed HFS-B disk',
                  'finder': 'separate executable loaded by System', 'checks': checks,
                  'qemu': subprocess.check_output(['qemu-system-riscv64', '--version'], text=True).splitlines()[0],
                  'disk_unchanged': True, 'disk_sha256': before,
                  'rom_sha256': hashlib.sha256((DIST / 'bootrom.bin').read_bytes()).hexdigest(),
                  'serial': describe(state / 'serial.log'),
                  'command': json.loads((state / 'command.json').read_text())}
        report_path.write_text(json.dumps(report, indent=2) + '\n')
        print(f'PASS: flash ROM -> blessed HFS-B System -> Finder; {len(checks)} desktop checks')
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
    global STATE, DIST
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    build_parser = sub.add_parser('build')
    launch_parser = sub.add_parser('start')
    launch_parser.add_argument('--frontend', choices=['auto', 'native', 'gtk', 'headless'], default='auto')
    launch_parser.add_argument('--no-build', action='store_true')
    launch_parser.add_argument('--qmp-port', type=int, default=4447,
                               help='Native Windows QMP port; choose another for a second VM')
    check_parser = sub.add_parser('check')
    check_parser.add_argument('--no-build', action='store_true')
    status_parser = sub.add_parser('status')
    stop_parser = sub.add_parser('stop')
    shot = sub.add_parser('screenshot')
    shot.add_argument('path')
    for command in (build_parser, launch_parser, check_parser, status_parser, stop_parser, shot):
        command.add_argument('--build-dir', type=Path,
                             help='Build/media root (contains dist/); defaults to a separate root for check')
        command.add_argument('--state-dir', type=Path,
                             help='Working VM state; custom builds default to BUILD_DIR/vm')
    args = parser.parse_args()
    if args.build_dir:
        os.environ[BUILD_DIR_ENV] = str(args.build_dir.resolve())
    elif args.action == 'check':
        os.environ[BUILD_DIR_ENV] = str(build_dir() / 'check')
    DIST = dist_dir()
    if args.state_dir:
        STATE = args.state_dir.resolve()
    elif build_dir() != ROOT / 'build/system7':
        STATE = build_dir() / 'vm'
    if args.action == 'build':
        build()
    elif args.action == 'start':
        start(args.frontend, not args.no_build, args.qmp_port)
    elif args.action == 'check':
        check(not args.no_build, args.state_dir.resolve() if args.state_dir else None)
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
