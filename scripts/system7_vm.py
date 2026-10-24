#!/usr/bin/env python3
"""Build and boot the System7 disk desktop through its read-only flash ROM and private working disk."""
from system7_platform import cpu_model
import argparse
import hashlib
import json
import os
import re
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
                   cwd=ROOT / 'System7', check=True,
                   env=dict(os.environ, SYSTEM7_VM_STATE_DIR=str(STATE)))


def launch(state, frontend='headless', extra_disks=(), *, writable=False, preserve=False,
           qmp_port=4447, host_bridge=None, expected_exports=None, vector_bits=None):
    """Launch a private disk copy. Tests default to read-only and fresh copies.

    Persistent desktop starts retain their writable copy while the build is unchanged.
    A new OS build preserves Preferences/Documents into the replacement before
    retiring that copy; obsolete boot disks are not retained.
    """
    state = Path(state).resolve()
    if host_bridge is not None and (type(host_bridge) is not int or not 0 < host_bridge < 65536):
        raise ValueError('host bridge must be an explicit loopback TCP port')
    if expected_exports is not None and (type(expected_exports) is not int
                                         or not 1 <= expected_exports <= 16
                                         or host_bridge is None):
        raise ValueError('expected exports require a host bridge and a count from 1 to 16')
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
    if preserve:
        import system7_user_state
    disk = state / 'system7.qcow2'
    base_hash = hashlib.sha256((DIST / 'system7.qcow2').read_bytes()).hexdigest()
    stamp = state / 'base-disk.sha256'
    same_base = stamp.exists() and stamp.read_text().strip() == base_hash
    if not disk.exists() or not same_base or not preserve:
        if preserve:
            system7_user_state.replace_disk(build_dir(), state, DIST / 'system7.qcow2')
        else:
            shutil.copyfile(DIST / 'system7.qcow2', disk)
        stamp.write_text(base_hash + '\n')
    elif preserve:
        system7_user_state.mark_persistent(build_dir(), state)
    shutil.copyfile(DIST / 'bootrom.bin', state / 'bootrom.bin')
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
    args = [executable, '-uuid', identity, '-name', 'System RISC - Finder', '-machine', 'virt', '-cpu', cpu_model(vector_bits),
            '-m', '128M', '-smp', '1', '-bios', 'none',
            '-drive', 'if=pflash,unit=0,format=raw,readonly=on,file=' + host_path(state / 'bootrom.bin'),
            '-drive', 'if=none,id=startup,format=qcow2,readonly=' + ('off' if writable else 'on') + ',file=' + host_path(disk),
            '-global', 'virtio-mmio.force-legacy=false',
            '-device', 'virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0',
            '-device', 'ramfb', '-device', 'virtio-keyboard-device', '-device', 'virtio-tablet-device',
            '-display', display, '-nic', 'none', '-serial', 'file:' + host_path(serial),
            '-parallel', 'none', '-monitor', 'none', '-qmp', monitor]
    if host_bridge is not None:
        # Bus 3 is reserved for this optional transport. Extra disks start at 4.
        # The listener owns the explicit exports; this never exports a default
        # workspace or accepts an arbitrary QEMU device/command string.
        args.extend(['-chardev', f'socket,id=hostbridge,host=127.0.0.1,port={host_bridge},reconnect-ms=100',
                     '-device', 'virtio-serial-device,bus=virtio-mmio-bus.3',
                     '-device', 'virtconsole,chardev=hostbridge'])
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
    # Native Windows QEMU reads WSL-hosted media through UNC paths. Its GTK
    # profile cold-boots this image in ~33 seconds, beyond the Linux budget.
    deadline = time.monotonic() + (60 if native else 25)
    try:
        while time.monotonic() < deadline:
            log = serial.read_text(errors='replace')
            if process.poll() is not None:
                raise RuntimeError((state / 'qemu.log').read_text() + '\n' + log)
            if 'DESKTOP_READY' in log:
                if expected_exports is not None:
                    mounted = {int(index) for index in re.findall(
                        r'^HOST_VOLUME_MOUNTED index=(\d+) ', log, flags=re.MULTILINE)}
                    if mounted != set(range(expected_exports)):
                        raise RuntimeError('Requested host volumes did not all mount:\n' + log)
                qmp('query-status', endpoint=endpoint)
                return process, endpoint
            if any(marker in log for marker in ['BOOT_NO_SYSTEM:', 'BOOT_TRAP:', 'BOOT_PANIC:', 'BOOT_UNSUPPORTED_ISA:',
                                               'BOOT_MEMORY_REJECTED:', 'SYSTEM_PANIC:', 'SYSTEM_TRAP:', 'SYSTEM_ALLOCATION_FAILED:']):
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


def start(frontend='auto', rebuild=True, qmp_port=4447, *, exports=(), readonly_exports=(), transfer=True):
    count = len(exports) + len(readonly_exports) + int(transfer)
    if count > 16:
        raise ValueError('select at most sixteen explicit host exports')
    try:
        status = qmp('query-status')
    except (OSError, ValueError, RuntimeError):
        pass
    else:
        if exports or readonly_exports:
            raise RuntimeError('VM is already running; stop it before changing host exports')
        print(json.dumps({'already_running': status}))
        return
    if rebuild:
        build()
    if frontend == 'auto':
        frontend = 'native' if native_executable() else 'gtk'
    companion = None
    port = None
    initialize = None
    if transfer:
        # Private build profiles get private exports; tests never touch the
        # user's persistent source-root transfer volume.
        initialize = ((ROOT / 'System7') if build_dir() == ROOT / 'build/system7'
                      else build_dir()) / 'transfer'
        exports = [f'Transfer={initialize}', *exports]
    try:
        if count:
            import system7_exports
            companion, port = system7_exports.start(STATE, exports, readonly_exports,
                                                     initialize=initialize)
        STATE.mkdir(parents=True, exist_ok=True)
        (STATE / 'host-exports.json').write_text(json.dumps({
            'exports': list(exports), 'read_only_exports': list(readonly_exports),
            'companion_pid': companion.pid if companion is not None else None,
            'port': port}, indent=2) + '\n')
        process, endpoint = launch(STATE, frontend, writable=True, preserve=True,
                                   qmp_port=qmp_port, host_bridge=port,
                                   expected_exports=count or None)
    except BaseException:
        if companion is not None:
            system7_exports.stop(companion)
        raise
    print(json.dumps({'pid': process.pid, 'frontend': 'native-gtk' if frontend == 'native' else frontend,
                      'bootrom': describe(STATE / 'bootrom.bin'),
                      'disk': describe(STATE / 'system7.qcow2'), 'build_dir': str(build_dir())}))
    print((STATE / 'serial.log').read_text())
    return process, endpoint


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
    launch_parser.add_argument('--no-transfer', action='store_true',
                               help='Disable the default Transfer volume for this VM')
    from system7_exports import export_argument
    launch_parser.add_argument('--export', action='append', default=[], type=export_argument,
                               metavar='NAME=PATH', help='Expose a prepared read/write host volume (repeatable)')
    launch_parser.add_argument('--read-only-export', action='append', default=[], type=export_argument,
                               metavar='NAME=PATH', help='Expose a prepared read-only host volume (repeatable)')
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
        start(args.frontend, not args.no_build, args.qmp_port,
              exports=args.export, readonly_exports=args.read_only_export,
              transfer=not args.no_transfer)
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
