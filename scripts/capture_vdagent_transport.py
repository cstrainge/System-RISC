#!/usr/bin/env python3
"""Collect private native INIT -> VBYT -> virtio -> vdagent evidence.

The default cases have no graphical host clipboard peer. The optional private
X11 case owns a fresh Xvfb and fixed-literal selection peer, never the caller's
DISPLAY. Rust owns acceptance; none of these cases activates a production worker.
"""
from contextlib import contextmanager
import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import threading
from unittest.mock import patch

from hfs_b_tool import Image
from system7_platform import TARGET, guest_env
import system7_vm as vm
from system7_capture import stop_owned

MODES = ('capabilities', 'wrong-name', 'malformed', 'absent')
DISK_SUFFIXES = ('.raw', '.qcow2', '.qcow', '.hfs', '.hfsb', '.img')


def environment(guest=False):
    selected = guest_env() if guest else dict(os.environ)
    selected[vm.BUILD_DIR_ENV] = str(vm.build_dir())
    return selected


def run(arguments, guest=False):
    return subprocess.run(arguments, cwd=vm.SOURCE, env=environment(guest), check=True)


def prepare(output, private_text=False):
    manifest = vm.SOURCE / 'crates/mac-toolbox/tests/fixtures/vdagent-transport-probe/Cargo.toml'
    target = vm.build_dir() / 'fixtures/vdagent-transport-probe'
    target.mkdir(parents=True, exist_ok=True)
    features = ['--features', 'private-text'] if private_text else []
    packages = output / ('text-packages' if private_text else 'packages')
    with (target / 'package.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        run(['cargo', 'build', '--locked', '--manifest-path', str(manifest), '--release',
             '--target', TARGET, '--bin', 'VDAgentTransportProbe', '--target-dir', str(target), *features], guest=True)
        run(['cargo', 'run', '--locked', '--manifest-path', str(manifest), '--bin', 'package',
             '--target-dir', str(target), *features, '--', str(target / TARGET / 'release/VDAgentTransportProbe'),
             str(packages)])


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class MalformedPeer:
    """Private loopback bytes only, with no OS clipboard integration."""
    def __init__(self):
        self.listener = socket.socket()
        self.connection = None
        self.stopping = threading.Event()
        self.errors = []
        self.sent = b''
        self.received = bytearray()
        try:
            self.listener.bind(('127.0.0.1', 0))
            self.listener.listen(1)
            self.listener.settimeout(.1)
            self.port = self.listener.getsockname()[1]
            self.thread = threading.Thread(target=self.serve, daemon=True)
            self.thread.start()
        except Exception as error:
            try:
                self.listener.close()
            except Exception as cleanup:
                raise ExceptionGroup('Private malformed-peer startup and cleanup failed',
                                     [error, cleanup]) from error
            raise

    def serve(self):
        try:
            while not self.stopping.is_set():
                try:
                    self.connection, _ = self.listener.accept()
                    break
                except socket.timeout:
                    continue
            if self.connection is None:
                return
            self.connection.settimeout(.2)
            # Complete standard chunk containing an unsupported protocol=2
            # message header. Only the native Rust decoder decides refusal.
            self.sent = struct.pack('<II', 1, 28) + struct.pack('<IIQI', 2, 6, 0, 8) + struct.pack('<II', 0, 0)
            self.connection.sendall(self.sent)
            while not self.stopping.is_set():
                try:
                    data = self.connection.recv(4096)
                except socket.timeout:
                    continue
                if not data:
                    break
                self.received.extend(data)
        except OSError as error:
            if not self.stopping.is_set():
                self.errors.append(str(error))
        finally:
            # The accepting thread retains ownership even if close() runs
            # between accept() returning and publishing self.connection.
            if self.connection is not None:
                try:
                    self.connection.close()
                except OSError as error:
                    self.errors.append(str(error))

    def close(self):
        self.stopping.set()
        errors = []
        try:
            self.listener.close()
        except Exception as error:
            errors.append(error)
        if self.connection is not None:
            try:
                self.connection.shutdown(socket.SHUT_RDWR)
            except OSError as error:
                if error.errno not in (errno.EBADF, errno.ENOTCONN):
                    errors.append(error)
            try:
                self.connection.close()
            except Exception as error:
                errors.append(error)
        try:
            self.thread.join(timeout=3)
        except Exception as error:
            errors.append(error)
        if self.thread.is_alive():
            errors.append(RuntimeError('Private malformed-peer thread did not acknowledge shutdown'))
        if errors:
            raise ExceptionGroup(f'Private malformed-peer cleanup failed: {errors[0]}', errors)


@contextmanager
def native_channel(mode, peer, private_x11=None, qemu=None, owned=None):
    """Add an acceptance-only channel to the genuine private headless launcher."""
    popen = subprocess.Popen
    executable = str(qemu) if qemu is not None else 'qemu-system-riscv64'
    gtk_clipboard = vm.gtk_clipboard_schema(executable) if private_x11 is not None else False
    if private_x11 is not None:
        private_x11.clipboard_option = gtk_clipboard
    def spawn(arguments, *args, **options):
        arguments = list(arguments)
        arguments[0] = executable
        display = arguments.index('-display') + 1
        if '-uuid' not in arguments or (private_x11 is None and arguments[display] != 'none'):
            raise RuntimeError('Native vdagent capture requires its own UUID and headless display')
        selected = dict(options['env'])
        for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS', 'QEMU_DISPLAY'):
            selected.pop(name, None)
        selected[vm.BUILD_DIR_ENV] = str(vm.build_dir())
        if private_x11 is not None:
            selected.update(private_x11.environment())
            arguments[display] = 'gtk,gl=off,zoom-to-fit=off,show-menubar=off'
            if gtk_clipboard:
                arguments[display] += ',clipboard=on'
        options['env'] = selected
        if mode != 'absent':
            backend = ('socket,id=probeagent,host=127.0.0.1,port=' + str(peer.port)
                       if mode == 'malformed' else 'null,id=probeagent' if mode == 'wrong-name'
                       else 'qemu-vdagent,id=probeagent,clipboard=on,mouse=off')
            name = 'org.system-risc.absent-test' if mode == 'wrong-name' else 'com.redhat.spice.0'
            arguments.extend(['-chardev', backend,
                              '-device', 'virtio-serial-device,id=probeserial,bus=virtio-mmio-bus.4,max_ports=2',
                              '-device', 'virtserialport,bus=probeserial.0,nr=1,chardev=probeagent,name=' + name])
        serial = arguments[arguments.index('-serial') + 1]
        if not serial.startswith('file:'):
            raise RuntimeError('Native vdagent capture requires a private serial evidence file')
        (Path(serial[5:]).parent / 'native-command.json').write_text(json.dumps(arguments, indent=2) + '\n')
        process = popen(arguments, *args, **options)
        if owned is not None:
            # Retain ownership before vm.launch can raise without returning
            # its child handle or QMP endpoint to the capture caller.
            owned.append(process)
        return process
    with patch.object(vm.subprocess, 'Popen', side_effect=spawn):
        yield


def capture_case(output, mode, base, xvfb=None, qemu=None):
    output.mkdir()
    dist = output / 'dist'
    dist.mkdir()
    raw = dist / 'probe.raw'
    process = endpoint = peer = private_x11 = None
    owned = []
    original_dist = vm.DIST
    report = {'mode': mode, 'scope': 'native-bootstrap-transport-only',
              'host_clipboard_peer': mode == 'private-x11', 'host_display_environment_removed': True}
    errors = []
    try:
        run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k', str(base / 'system7.qcow2'), str(raw)])
        packages = 'text-packages' if mode == 'private-x11' else 'packages'
        Image(raw).import_macbinary(output.parent / packages / 'VDAgent Transport Probe.bin', '/System Folder/Extensions')
        run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw), str(dist / 'system7.qcow2')])
        import shutil
        shutil.copyfile(base / 'bootrom.bin', dist / 'bootrom.bin')
        if mode == 'malformed':
            peer = MalformedPeer()
        if mode == 'private-x11':
            if xvfb is None:
                raise ValueError('Private X11 text capture requires an explicit owned Xvfb executable (--xvfb)')
            from private_x11_clipboard import PrivateX11
            private_x11 = PrivateX11(xvfb, output / 'private-x11')
            private_x11.peer.ready_file = output / 'vm/serial.log'
        vm.DIST = dist
        with native_channel(mode, peer, private_x11, qemu=qemu, owned=owned):
            process, endpoint = vm.launch(output / 'vm', frontend='gtk' if private_x11 else 'headless', clipboard=False)
        report['termination'] = 'desktop-ready'
        report['serial'] = str(output / 'vm/serial.log')
        report['command'] = json.loads((output / 'vm/native-command.json').read_text())
        if mode != 'absent':
            # The public command log predates the private acceptance-only
            # injection. Record the exact additionally supplied profile too.
            report['native_channel'] = {'name': 'org.system-risc.absent-test' if mode == 'wrong-name' else 'com.redhat.spice.0',
                                        'max_ports': 2, 'nr': 1, 'bus': 4,
                                        'backend': 'socket' if peer else 'null' if mode == 'wrong-name' else 'qemu-vdagent'}
        if private_x11 is not None:
            from private_x11_clipboard import HOST_TEXT
            private_x11.peer.received_event.wait(timeout=5)
            report['private_text'] = {'scope': 'fresh-owned-Xvfb-only', 'display': private_x11.address,
                                      'server_pid': private_x11.process.pid,
                                      'host_text_utf8_hex': HOST_TEXT.encode().hex(),
                                      'host_utf8_requests': private_x11.peer.served_host_utf8,
                                      'host_reoffered_after_caps': private_x11.peer.reoffered,
                                      'gtk_clipboard_option': private_x11.clipboard_option,
                                      'guest_text_utf8_hex': None if private_x11.peer.received is None else private_x11.peer.received.hex(),
                                      'errors': private_x11.peer.errors,
                                      'relay_errors': private_x11.relay.errors}
    except Exception as error:
        report['capture_error'] = str(error)
        errors.append(error)
    finally:
        owners = []
        for child in owned:
            try:
                stop_owned(child, endpoint)
            except Exception as error:
                errors.append(error)
            try:
                exit_code = child.poll()
                reaped = exit_code is not None
            except Exception as error:
                errors.append(error)
                exit_code = None
                reaped = False
            owners.append({'pid': child.pid, 'reaped': reaped, 'exit_code': exit_code})
        if peer is not None:
            try:
                peer.close()
            except Exception as error:
                errors.append(error)
            report['private_peer'] = {'sent_hex': peer.sent.hex(), 'received_hex': bytes(peer.received).hex(), 'errors': peer.errors}
        if private_x11 is not None:
            # Preserve host-peer diagnostics even when launch/bootstrap fails.
            from private_x11_clipboard import HOST_TEXT
            report['private_text'] = {'scope': 'fresh-owned-Xvfb-only', 'display': private_x11.address,
                                      'server_pid': private_x11.process.pid,
                                      'host_text_utf8_hex': HOST_TEXT.encode().hex(),
                                      'host_target_requests': private_x11.peer.target_requests,
                                      'host_utf8_requests': private_x11.peer.served_host_utf8,
                                      'host_reoffered_after_caps': private_x11.peer.reoffered,
                                      'gtk_clipboard_option': getattr(private_x11, 'clipboard_option', False),
                                      'guest_text_utf8_hex': None if private_x11.peer.received is None else private_x11.peer.received.hex(),
                                      'errors': private_x11.peer.errors,
                                      'relay_errors': private_x11.relay.errors}
            try:
                private_x11.close()
            except Exception as error:
                errors.append(error)
        vm.DIST = original_dist
        media = sorted(path for path in output.rglob('*')
                       if path.is_file() and (path.suffix in DISK_SUFFIXES or path.name == 'bootrom.bin'))
        report['media_owners'] = owners
        if all(owner['reaped'] for owner in owners):
            for path in media:
                try:
                    path.unlink()
                except OSError as error:
                    errors.append(error)
        else:
            detail = ('VM media quarantined because an owned QEMU process could not be positively '
                      'reaped; retain its exact media and owner state until that process exits')
            report['media_quarantine'] = {'reason': detail, 'media': [str(path) for path in media],
                                           'state_dir': str(output / 'vm'), 'qmp_endpoint': endpoint}
            errors.append(RuntimeError(detail))
        report['cleanup_errors'] = [str(error) for error in errors]
        (output / 'case.json').write_text(json.dumps(report, indent=2) + '\n')
    if errors:
        raise ExceptionGroup('Private native vdagent capture failed; inspect case.json and serial evidence', errors)
    return report


def capture(output, modes=MODES, xvfb=None, qemu=None):
    os.environ[vm.BUILD_DIR_ENV] = str(vm.build_dir())
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    base = vm.build_dir() / 'dist'
    names = ('system7.qcow2', 'bootrom.bin', 'System.rsrc')
    before = {name: digest(base / name) for name in names}
    if any(mode != 'private-x11' for mode in modes):
        prepare(output)
    if 'private-x11' in modes:
        prepare(output, private_text=True)
    cases = {}
    errors = []
    for mode in modes:
        try:
            cases[mode] = capture_case(output / mode, mode, base, xvfb=xvfb, qemu=qemu)
        except Exception as error:
            errors.append(error)
            cases[mode] = json.loads((output / mode / 'case.json').read_text())
    report = {'scope': 'native-bootstrap-transport-only', 'build_dir': str(vm.build_dir()),
              'original_before': before, 'original_after': {name: digest(base / name) for name in names},
              'cases': cases}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    if errors:
        raise ExceptionGroup('Native vdagent evidence collection failed', errors)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--mode', action='append', choices=(*MODES, 'private-x11'))
    parser.add_argument('--xvfb', type=Path, help='Explicit Xvfb executable for optional isolated private-x11 text case')
    parser.add_argument('--qemu', type=Path, help='Private QEMU executable; never installs or replaces the normal host binary')
    args = parser.parse_args()
    capture(args.output.resolve(), args.mode or MODES, xvfb=args.xvfb, qemu=args.qemu)
