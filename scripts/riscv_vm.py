#!/usr/bin/env python3
"""Build, boot, inspect, and stop the independent RISC-V QuickDraw demo."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / 'working/riscv-vm'
TARGET = 'riscv64imac-unknown-none-elf'
BUILD = ROOT / 'build/riscv-quickdraw'
KERNEL = BUILD / TARGET / 'release/quickdraw-kernel'


def windows_path(path):
    return subprocess.check_output(['wslpath', '-w', str(Path(path).resolve())], text=True).strip()


def qmp(command, arguments=None, endpoint=None):
    if endpoint is None:
        endpoint = json.loads((STATE / 'qmp-endpoint.json').read_text())
    if endpoint['transport'] == 'tcp':
        sock = socket.create_connection((endpoint['host'], endpoint['port']), timeout=.5)
    else:
        sock = socket.socket(socket.AF_UNIX)
        sock.settimeout(.5)
        sock.connect(endpoint['path'])
    arguments = dict(arguments or {})
    if endpoint.get('platform') == 'windows' and command == 'screendump':
        arguments['filename'] = windows_path(arguments['filename'])
    with sock:
        sock.settimeout(5)
        stream = sock.makefile('rwb', buffering=0)
        json.loads(stream.readline())
        def execute(name, args, ident):
            stream.write((json.dumps({'execute': name, 'arguments': args, 'id': ident})+'\n').encode())
            while True:
                response = json.loads(stream.readline())
                if response.get('id') == ident:
                    if 'error' in response:
                        raise RuntimeError(response['error'])
                    return response['return']
        execute('qmp_capabilities', {}, 1)
        return execute(command, arguments, 2)


def build():
    subprocess.run(['cargo', 'build', '--manifest-path', str(ROOT/'riscv-quickdraw/Cargo.toml'),
                    '--target-dir', str(BUILD), '--release', '--target', TARGET,
                    '--features', 'bare-metal', '--bin', 'quickdraw-kernel'], check=True)
    return KERNEL


def native_executable():
    config = ROOT / 'working/vm/windows-qemu.json'
    if config.exists():
        exe = Path(json.loads(config.read_text())['executable']).with_name('qemu-system-riscv64.exe')
        if exe.exists():
            return exe
    return None


def launch(state, frontend='headless', animate=True):
    state.mkdir(parents=True, exist_ok=True)
    native = frontend == 'native'
    if native and not native_executable():
        raise FileNotFoundError('Portable qemu-system-riscv64.exe missing; use --frontend gtk')
    executable = str(native_executable()) if native else 'qemu-system-riscv64'
    host_path = windows_path if native else str
    endpoint = ({'transport':'tcp', 'host':'127.0.0.1', 'port':4446, 'platform':'windows'} if native
                else {'transport':'unix', 'path':str(state/'qmp.sock')})
    socket_path = state/'qmp.sock'
    if not native and socket_path.exists():
        socket_path.unlink()
    monitor = 'tcp:127.0.0.1:4446,server=on,wait=off' if native else f'unix:{socket_path},server=on,wait=off'
    # GTK owns both zoom and input scaling. SDL's 2D renderer and QEMU each
    # scale absolute coordinates, confining a 2x window's input to half the guest.
    display = {'native':'gtk,zoom-to-fit=off,scale=2,show-menubar=off',
               'gtk':'gtk,zoom-to-fit=off,scale=2', 'headless':'none'}[frontend]
    serial = state / 'serial.log'
    serial.write_text('')
    args = [executable, '-name', 'REA - RISC-V QuickDraw', '-machine', 'virt', '-cpu', 'rv64',
            '-m', '128M', '-smp', '1', '-bios', 'none', '-kernel', host_path(KERNEL),
            '-device', 'ramfb', '-global', 'virtio-mmio.force-legacy=false',
            '-device', 'virtio-keyboard-device', '-device', 'virtio-tablet-device',
            '-display', display, '-nic', 'none',
            '-serial', 'file:'+host_path(serial), '-parallel', 'none', '-monitor', 'none', '-qmp', monitor,
            '-fw_cfg', 'name=opt/rea/animate,string='+('1' if animate else '0')]
    env = dict(os.environ, GDK_BACKEND='x11')
    if native:
        settings = {'GDK_BACKEND':'win32', 'GDK_SCALE':'1'}
        env.update(settings)
        env['WSLENV'] = ':'.join(filter(None, [env.get('WSLENV',''), *settings]))
    (state/'command.json').write_text(json.dumps(args, indent=2)+'\n')
    (state/'qmp-endpoint.json').write_text(json.dumps(endpoint, indent=2)+'\n')
    with (state/'qemu.log').open('wb') as log:
        process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   env=env, start_new_session=True)
    (state/'pid').write_text(str(process.pid)+'\n')
    deadline = time.monotonic()+20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError((state/'qemu.log').read_text())
        if 'BOOT_OK' in serial.read_text(errors='replace'):
            qmp('query-status', endpoint=endpoint)
            return process, endpoint
        time.sleep(.1)
    process.terminate()
    raise TimeoutError('Guest did not finish boot:\n'+serial.read_text(errors='replace')+'\n'+(state/'qemu.log').read_text())


def start(frontend='auto', animate=True):
    try:
        status = qmp('query-status')
    except (OSError, ValueError):
        pass
    else:
        print(json.dumps({'already_running':status}))
        return
    build()
    if frontend == 'auto':
        frontend = 'native' if native_executable() else 'gtk'
    process, _ = launch(STATE, frontend, animate)
    print(json.dumps({'pid':process.pid, 'frontend':'native-gtk' if frontend=='native' else frontend,
                      'kernel':str(KERNEL.relative_to(ROOT))}))
    print((STATE/'serial.log').read_text())


def check():
    # A separate headless instance leaves either graphical VM undisturbed.
    from PIL import Image, ImageChops
    subprocess.run(['cargo', 'test', '--manifest-path', str(ROOT/'riscv-quickdraw/Cargo.toml'),
                    '--target-dir', str(BUILD), '--lib'], check=True)
    build()
    results = ROOT/'results/riscv-quickdraw'
    results.mkdir(parents=True, exist_ok=True)
    host_frame = results/'host.ppm'
    host = subprocess.check_output(['cargo', 'run', '--manifest-path', str(ROOT/'riscv-quickdraw/Cargo.toml'),
        '--target-dir', str(BUILD), '--release', '--features', 'host', '--bin', 'quickdraw-frame', '--', str(host_frame)], text=True)
    process, endpoint = launch(results/'static-vm', 'headless', False)
    try:
        screenshot = results/'guest.png'
        qmp('screendump', {'filename':str(screenshot), 'format':'png'}, endpoint)
        guest_log = (results/'static-vm/serial.log').read_text()
        assert 'QUICKDRAW_SELF_CHECKS=PASS' in guest_log
        checksum = host.strip().split('framebuffer_fnv1a=')[1]
        assert f'FIRST_FRAME_FNV1A={checksum}' in guest_log
        with Image.open(host_frame) as expected, Image.open(screenshot) as actual:
            expected = expected.convert('RGB'); actual = actual.convert('RGB')
            assert expected.size == actual.size == (640,480)
            assert ImageChops.difference(expected, actual).getbbox() is None, 'host and guest pixels differ'
        from check_riscv_windows import check_events
        window_checks=check_events(endpoint,results/'static-vm/serial.log',results)
        record = {'kernel':str(KERNEL.relative_to(ROOT)), 'kernel_sha256':hashlib.sha256(KERNEL.read_bytes()).hexdigest(),
                  'qemu':subprocess.check_output(['qemu-system-riscv64','--version'],text=True).splitlines()[0],
                  'host_unit_tests':'passed', 'guest_self_checks':'passed', 'cold_boot':'passed',
                  'framebuffer_pixels':'all 307200 RGB pixels match host reference',
                  'framebuffer_fnv1a':checksum, 'screenshot':str(screenshot.relative_to(ROOT)),
                  'window_event_checks':{'passed':len(window_checks),'results':str((results/'window-checks.json').relative_to(ROOT))},
                  'sources':['https://www.qemu.org/docs/master/specs/fw_cfg.html',
                             'https://github.com/qemu/qemu/blob/v10.1.0/hw/display/ramfb.c',
                             'https://developer.apple.com/library/archive/documentation/mac/pdf/Imaging_With_QuickDraw/QuickDraw_Drawing.pdf'],
                  'scope':'Native Rust QuickDraw, rectangular Window Manager and Event Manager subsets with virtio keyboard/tablet input. No System/Finder, 68k translator, or native guest trap ABI yet. Original Mac differential validation is pending.'}
        (ROOT/'provenance/riscv-quickdraw.json').write_text(json.dumps(record,indent=2)+'\n')
        print('PASS: bare-metal RISC-V boot, geometry checks, checksum, and exact graphical framebuffer comparison')
    finally:
        try: qmp('quit', endpoint=endpoint)
        finally: process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('build')
    launch_parser = sub.add_parser('start')
    launch_parser.add_argument('--frontend', choices=['auto','native','gtk','headless'], default='auto')
    launch_parser.add_argument('--static', action='store_true')
    sub.add_parser('check')
    sub.add_parser('status')
    sub.add_parser('stop')
    shot = sub.add_parser('screenshot'); shot.add_argument('path')
    args = parser.parse_args()
    if args.action == 'build': print(build())
    elif args.action == 'start': start(args.frontend, not args.static)
    elif args.action == 'check': check()
    elif args.action == 'status': print(json.dumps(qmp('query-status')))
    elif args.action == 'stop': print(json.dumps(qmp('quit')))
    elif args.action == 'screenshot':
        path = Path(args.path).resolve(); path.parent.mkdir(parents=True, exist_ok=True)
        print(json.dumps(qmp('screendump', {'filename':str(path), 'format':'png'})))


if __name__ == '__main__':
    main()
