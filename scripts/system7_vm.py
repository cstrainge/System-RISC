#!/usr/bin/env python3
"""Build and boot the System7 disk desktop through its read-only flash ROM and private working disk."""
from system7_platform import RAM, cpu_model, display_size
import argparse
import hashlib
import json
import math
import os
import re
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

from riscv_vm import native_executable, qmp as send_qmp, windows_path
from system7_paths import SOURCE, ROOT, BUILD_DIR_ENV, build_dir, dist_dir, describe
from system7_host import process_options

STATE = ROOT / 'working/system7-vm'
DIST = dist_dir()

COCOA_SCREEN_SCRIPT = '''ObjC.import("AppKit");
const screen = $.NSScreen.mainScreen;
if (screen.isNil()) throw Error("No active Mac screen");
const style = $.NSWindowStyleMaskTitled | $.NSWindowStyleMaskMiniaturizable | $.NSWindowStyleMaskClosable;
const content = $.NSWindow.contentRectForFrameRectStyleMask(screen.visibleFrame, style);
JSON.stringify({width: content.size.width, height: content.size.height});'''


def cocoa_scale():
    """Prefer 2x only when the whole titled window fits the usable Mac screen."""
    width, height = display_size()
    try:
        output = subprocess.check_output(['/usr/bin/osascript', '-l', 'JavaScript', '-e',
                                          COCOA_SCREEN_SCRIPT],
                                         text=True, stderr=subprocess.PIPE, timeout=10)
        size = json.loads(output)
        if not isinstance(size, dict) or any(
                type(size.get(axis)) not in (int, float) or not math.isfinite(size[axis])
                or size[axis] <= 0 for axis in ('width', 'height')):
            raise ValueError('usable screen size must contain finite positive width and height')
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        cause = getattr(error, 'stderr', None) or str(error)
        print(f'Unable to measure the Mac screen; launching at 1x: {cause.strip()}', file=sys.stderr)
        return 1
    return 2 if width * 2 <= size['width'] and height * 2 <= size['height'] else 1


def qmp(command, arguments=None, endpoint=None):
    if endpoint is None:
        endpoint = json.loads((STATE / 'qmp-endpoint.json').read_text())
    return send_qmp(command, arguments, endpoint)


def build():
    subprocess.run(['cargo', 'xtask', 'image', '--build-dir', str(build_dir())],
                   cwd=SOURCE, check=True,
                   env=dict(os.environ, SYSTEM7_VM_STATE_DIR=str(STATE), SYSTEM7_PYTHON=sys.executable))


def clipboard_backend(executable):
    """Inspect capabilities only; never read or change the host clipboard."""
    try:
        result = subprocess.run([str(executable), '-machine', 'none', '-nodefaults',
                                 '-display', 'none', '-chardev', 'help'],
                                capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(f'Cannot check QEMU clipboard backend: {error}') from error
    output = result.stdout + result.stderr
    if result.returncode != 0 or not re.search(r'^\s*qemu-vdagent\s*$', output, re.MULTILINE):
        cause = output.strip()[-2000:]
        raise RuntimeError('Selected QEMU does not advertise the qemu-vdagent clipboard backend; '
                           'install QEMU with qemu-vdagent and a clipboard-capable graphical display'
                           + (f':\n{cause}' if cause else ''))


def qmp_schema(executable):
    """Query a CPU-less QEMU over an owned loopback connection, then reap it.

    Windows QEMU's console stdio transport cannot reliably consume piped QMP.
    The listener retains its ephemeral port until QEMU connects as a client;
    no existing monitor, display, disk or clipboard is opened.
    """
    process = None
    with tempfile.TemporaryFile() as errors, socket.socket() as listener:
        try:
            listener.bind(('127.0.0.1', 0))
            listener.listen(1)
            listener.settimeout(10)
            port = listener.getsockname()[1]
            process = subprocess.Popen([str(executable), '-machine', 'none', '-nodefaults',
                                        '-display', 'none', '-S', '-qmp', f'tcp:127.0.0.1:{port}',
                                        '-monitor', 'none', '-serial', 'none'],
                                       stdin=subprocess.DEVNULL, stdout=errors, stderr=errors,
                                       **process_options())
            connection, _ = listener.accept()
            with connection:
                deadline = time.monotonic() + 10
                pending = bytearray()
                limit = 16 * 1024 * 1024
                def budget():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError('QMP schema exchange exceeded its 10-second deadline')
                    connection.settimeout(remaining)
                def receive():
                    budget()
                    while b'\n' not in pending:
                        if len(pending) >= limit:
                            raise ValueError('QMP response exceeded 16 MiB')
                        budget()
                        data = connection.recv(min(65536, limit - len(pending)))
                        if not data:
                            raise ValueError('QMP response ended early')
                        pending.extend(data)
                    end = pending.index(b'\n') + 1
                    line = bytes(pending[:end])
                    del pending[:end]
                    value = json.loads(line)
                    if not isinstance(value, dict):
                        raise ValueError('QMP response must be an object')
                    return value
                if 'QMP' not in receive():
                    raise ValueError('selected QEMU did not send its QMP greeting')
                def execute(command, wait_result=True):
                    budget()
                    connection.sendall((json.dumps({'execute': command, 'id': command}) + '\n').encode())
                    if not wait_result:
                        return
                    for _ in range(64):
                        response = receive()
                        if response.get('id') == command:
                            if 'error' in response:
                                raise RuntimeError(str(response['error']))
                            if 'return' not in response:
                                raise ValueError(f'QMP {command} omitted its result')
                            return response['return']
                    raise ValueError(f'QMP {command} exceeded 64 unrelated messages')
                execute('qmp_capabilities')
                schema = execute('query-qmp-schema')
                # Some QEMU versions close the socket before delivering
                # quit's reply. Successful process exit acknowledges quit.
                execute('quit', wait_result=False)
            process.wait(timeout=5)
            if process.returncode != 0 or not isinstance(schema, list):
                raise ValueError('selected QEMU did not return a valid GTK clipboard schema')
            return schema
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
            errors.seek(0)
            detail = errors.read().decode('utf-8', errors='replace').strip()[-2000:]
            raise RuntimeError(f'Cannot query the selected QEMU GTK clipboard schema: {error}'
                               + (f': {detail}' if detail else '')) from error
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def gtk_clipboard_schema(executable):
    """GTK clipboard is an explicit runtime field beginning with QEMU 11.1."""
    schema = qmp_schema(executable)
    # Introspection deliberately masks non-ABI type names with opaque IDs.
    # Follow query-display-options -> tagged DisplayOptions -> gtk variant.
    records = {record['name']: record for record in schema
               if isinstance(record, dict) and isinstance(record.get('name'), str)}
    command = records.get('query-display-options', {})
    display = records.get(command.get('ret-type'), {})
    variants = display.get('variants', [])
    if not isinstance(variants, list):
        raise RuntimeError('Selected QEMU returned malformed GTK display variants')
    gtk = next((variant for variant in variants
                if isinstance(variant, dict) and variant.get('case') == 'gtk'), None)
    if gtk is None:
        return False
    members = records.get(gtk.get('type'), {}).get('members', [])
    if not isinstance(members, list):
        raise RuntimeError('Selected QEMU returned malformed GTK clipboard members')
    for member in members:
        if isinstance(member, dict) and member.get('name') == 'clipboard':
            value_type = records.get(member.get('type'), {})
            return value_type.get('meta-type') == 'builtin' and value_type.get('json-type') == 'boolean'
    return False


def select_clipboard(frontend, preference):
    if preference is not None and type(preference) is not bool:
        raise ValueError('clipboard must be automatic (None) or an explicit boolean')
    if preference is False:
        return False, 'Host clipboard sharing was explicitly disabled with --no-clipboard'
    if frontend not in ('gtk', 'native', 'cocoa'):
        return False, 'Headless display has no host clipboard peer; guest clipboard stays local'
    executable = native_executable() if frontend == 'native' else 'qemu-system-riscv64'
    try:
        if executable is None:
            raise RuntimeError('Windows QEMU is missing; install it or set SYSTEM7_WINDOWS_QEMU')
        clipboard_backend(executable)
        if frontend in ('gtk', 'native') and not gtk_clipboard_schema(executable):
            raise RuntimeError('Selected QEMU GTK has no declared clipboard facility; use QEMU '
                               'with the DisplayGTK.clipboard schema (11.1 or newer). '
                               'Legacy GTK builds remain local-only until separately qualified')
    except RuntimeError as error:
        reason = str(error)
        print('Host clipboard unavailable; keeping the guest clipboard local: ' + reason,
              file=sys.stderr)
        return False, reason
    return True, 'QEMU advertises the standard vdagent backend; graphical clipboard peer requested'


def launch(state, frontend='headless', extra_disks=(), *, writable=False, preserve=False,
           qmp_port=4447, host_bridge=None, expected_exports=None, vector_bits=None,
           clipboard=None):
    """Launch a private disk copy. Tests default to read-only and fresh copies.

    Persistent desktop starts retain their writable copy while the build is unchanged.
    A new OS build preserves Preferences/Documents into the replacement before
    retiring that copy; obsolete boot disks are not retained.
    """
    state = Path(state).resolve()
    if os.name == 'nt':
        frontend = {'headless': 'native-headless', 'gtk': 'native'}.get(frontend, frontend)
    clipboard, clipboard_reason = select_clipboard(frontend, clipboard)
    extra_disks = tuple(extra_disks)
    extra_limit = 3 if clipboard else 4
    if len(extra_disks) > extra_limit:
        raise ValueError(f'At most {extra_limit} additional disks fit beside graphics/input'
                         + (' and clipboard devices; use --no-clipboard for four extra disks' if clipboard else ' devices'))
    if host_bridge is not None and (type(host_bridge) is not int or not 0 < host_bridge < 65536):
        raise ValueError('host bridge must be an explicit loopback TCP port')
    if expected_exports is not None and (type(expected_exports) is not int
                                         or not 1 <= expected_exports <= 16
                                         or host_bridge is None):
        raise ValueError('expected exports require a host bridge and a count from 1 to 16')
    cocoa_zoom = cocoa_scale() if frontend == 'cocoa' else 1
    state.mkdir(parents=True, exist_ok=True)
    native = frontend in ('native', 'native-headless')
    if native and not native_executable():
        raise FileNotFoundError('qemu-system-riscv64.exe missing; install Windows QEMU or set SYSTEM7_WINDOWS_QEMU')
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
    # Cocoa owns native macOS window/input scaling; GTK options do not apply.
    # Its fixed scale option is provided by scripts/qemu/cocoa-scale.patch.
    display = {'native': 'gtk,gl=on,zoom-to-fit=off,scale=2,show-menubar=off',
               'gtk': 'gtk,gl=on,zoom-to-fit=off,scale=2', 'headless': 'none',
               'cocoa': f'cocoa,zoom-to-fit=off,scale={cocoa_zoom}',
               'native-headless': 'none'}[frontend]
    if clipboard and frontend in ('gtk', 'native'):
        # Selection was qualified against this exact binary's QMP schema.
        display += ',clipboard=on'
    serial = state / 'serial.log'
    serial.write_text('')
    identity = str(uuid.uuid4())
    args = [executable, '-uuid', identity, '-name', 'System RISC - Finder', '-machine', 'virt', '-cpu', cpu_model(vector_bits),
            '-m', RAM, '-smp', '1', '-bios', 'none',
            '-drive', 'if=pflash,unit=0,format=raw,readonly=on,file=' + host_path(state / 'bootrom.bin'),
            '-drive', 'if=none,id=startup,format=qcow2,readonly=' + ('off' if writable else 'on') + ',file=' + host_path(disk),
            '-global', 'virtio-mmio.force-legacy=false',
            '-device', 'virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0',
            '-device', 'ramfb',
            '-device', 'virtio-keyboard-device,bus=virtio-mmio-bus.1',
            '-device', 'virtio-tablet-device,bus=virtio-mmio-bus.2',
            '-display', display, '-nic', 'none', '-serial', 'file:' + host_path(serial),
            '-parallel', 'none', '-monitor', 'none', '-qmp', monitor]
    if host_bridge is not None:
        # A dedicated named controller is the admitted transport profile.
        # The listener owns the explicit exports; this never exports a default
        # workspace or accepts an arbitrary QEMU device/command string.
        args.extend(['-chardev', f'socket,id=hostbridge,host=127.0.0.1,port={host_bridge},reconnect-ms=100',
                     '-device', 'virtio-serial-device,id=hostserial,bus=virtio-mmio-bus.3,max_ports=2',
                     '-device', 'virtserialport,bus=hostserial.0,nr=1,chardev=hostbridge,name=org.system-risc.host-filesystem'])
    if clipboard:
        args.extend(['-chardev', 'qemu-vdagent,id=clipboard,clipboard=on,mouse=off',
                     '-device', 'virtio-serial-device,id=clipboardserial,bus=virtio-mmio-bus.4,max_ports=2',
                     '-device', 'virtserialport,bus=clipboardserial.0,nr=1,chardev=clipboard,name=com.redhat.spice.0'])
    for index, source in enumerate(extra_disks, start=1):
        source = Path(source).resolve()
        destination = state / f'extra-{index}.raw'
        shutil.copyfile(source, destination)
        args.extend(['-drive',f'if=none,id=extra{index},format=raw,readonly=on,file='+host_path(destination),
                     '-device',f'virtio-blk-device,drive=extra{index},bus=virtio-mmio-bus.{index+3+int(clipboard)}'])
    env = dict(os.environ)
    if frontend == 'gtk':
        env['GDK_BACKEND'] = 'x11'
    if native:
        settings = {'GDK_BACKEND': 'win32', 'GDK_SCALE': '1'}
        env.update(settings)
        env['WSLENV'] = ':'.join(filter(None, [env.get('WSLENV', ''), *settings]))
    (state / 'clipboard.json').write_text(json.dumps({'configured': clipboard, 'reason': clipboard_reason}, indent=2) + '\n')
    (state / 'command.json').write_text(json.dumps(args, indent=2) + '\n')
    (state / 'qmp-endpoint.json').write_text(json.dumps(endpoint, indent=2) + '\n')
    with (state / 'qemu.log').open('wb') as log:
        process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   env=env, **process_options())
    (state / 'pid').write_text(str(process.pid) + '\n')
    # Native Windows QEMU reads WSL-hosted media through UNC paths. Its GTK
    # profile cold-boots this image in ~33 seconds, beyond the Linux budget.
    deadline = time.monotonic() + (60 if native else 25)
    try:
        while time.monotonic() < deadline:
            log = serial.read_text(encoding='utf-8', errors='replace')
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
                                               'BOOT_MEMORY_REJECTED:', 'SYSTEM_STARTUP_FAILED:', 'SYSTEM_PANIC:', 'SYSTEM_TRAP:', 'SYSTEM_ALLOCATION_FAILED:']):
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


def start(frontend='auto', rebuild=True, qmp_port=4447, *, exports=(), readonly_exports=(), transfer=True,
          clipboard=None):
    if clipboard is not None and type(clipboard) is not bool:
        raise ValueError('clipboard must be automatic (None) or an explicit boolean')
    count = len(exports) + len(readonly_exports) + int(transfer)
    if count > 16:
        raise ValueError('select at most sixteen explicit host exports')
    try:
        status = qmp('query-status')
    except (OSError, ValueError, RuntimeError):
        pass
    else:
        if exports or readonly_exports or clipboard is not None:
            raise RuntimeError('VM is already running; stop it before changing host exports or clipboard sharing')
        print(json.dumps({'already_running': status}))
        return
    if frontend == 'auto':
        if sys.platform == 'darwin':
            frontend = 'cocoa'
        else:
            frontend = 'native' if os.name == 'nt' or native_executable() else 'gtk'
    if rebuild:
        build()
    companion = None
    port = None
    initialize = None
    if transfer:
        # Private build profiles get private exports; tests never touch the
        # user's persistent source-root transfer volume.
        initialize = (SOURCE if build_dir() == ROOT / 'build/system7'
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
                                   expected_exports=count or None, clipboard=clipboard)
    except BaseException:
        if companion is not None:
            system7_exports.stop(companion)
        raise
    print(json.dumps({'pid': process.pid, 'frontend': 'native-gtk' if frontend == 'native' else frontend,
                      'clipboard_policy': 'automatic' if clipboard is None else 'enabled' if clipboard else 'disabled',
                      'bootrom': describe(STATE / 'bootrom.bin'),
                      'disk': describe(STATE / 'system7.qcow2'), 'build_dir': str(build_dir())}))
    print((STATE / 'serial.log').read_text(encoding='utf-8', errors='replace'))
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
    if os.name == 'nt':
        for stream in (sys.stdout, sys.stderr):
            if hasattr(stream, 'reconfigure'):
                stream.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    build_parser = sub.add_parser('build')
    launch_parser = sub.add_parser('start')
    launch_parser.add_argument('--frontend', choices=['auto', 'native', 'native-headless', 'cocoa', 'gtk', 'headless'], default='auto')
    launch_parser.add_argument('--no-build', action='store_true')
    launch_parser.add_argument('--no-transfer', action='store_true',
                               help='Disable the default Transfer volume for this VM')
    launch_parser.add_argument('--no-clipboard', action='store_true',
                               help='Disable automatic host clipboard sharing; keep the guest clipboard local')
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
    if os.name == 'nt':
        # The guest and Windows host adapter use nightly APIs. Keep a caller's
        # explicit toolchain, and propagate this interpreter through xtask.
        os.environ.setdefault('RUSTUP_TOOLCHAIN', 'nightly')
        os.environ['SYSTEM7_PYTHON'] = sys.executable
        if args.action in ('start', 'build', 'check'):
            executable = native_executable()
            if executable is None:
                parser.error('Windows QEMU is required; install it or set SYSTEM7_WINDOWS_QEMU')
            os.environ['PATH'] = str(executable.parent) + os.pathsep + os.environ.get('PATH', '')
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
              transfer=not args.no_transfer, clipboard=False if args.no_clipboard else None)
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
