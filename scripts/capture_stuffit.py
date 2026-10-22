#!/usr/bin/env python3
"""Private public-SDK classic StuffIt transport; Rust owns assertions."""
import argparse
import hashlib
import json
import select
import re
from pathlib import Path
import shutil
import subprocess
import time

from capture_system7_applications_ui import Driver
from capture_system7_event_trace import decode
from hfs_b_tool import Image
import system7_vm as vm


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def trace_snapshot(output, endpoint, serial, prefix):
    marker = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=1',
                       serial.read_text(errors='replace'))
    if not marker:
        return
    address, size = map(int, marker.groups())
    path = output / (prefix + '-trace.bin')
    vm.qmp('stop', endpoint=endpoint)
    try:
        vm.qmp('pmemsave', {'val': address, 'size': size, 'filename': str(path)}, endpoint)
    finally:
        vm.qmp('cont', endpoint=endpoint)
    (output / (prefix + '-trace.json')).write_text(json.dumps(decode(path), indent=2))


def capture(output, without_driver=False, finder=False, finder_error=False):
    output.mkdir(parents=True, exist_ok=False)
    target = vm.build_dir() / 'fixtures/modules'
    for fixture, binary in ([] if finder else [('stuffit-app', 'Finder')]):
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                        '--target-dir', str(target)], check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(vm.build_dir() / 'host'), '--',
                        str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                        str(output / 'packages')], check=True)
    base = vm.DIST
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')], cwd=vm.ROOT / 'System7', check=True)
    before = {name: digest(base / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    dist = output / 'dist'
    dist.mkdir()
    raw = dist / 'stuffit.raw'
    process = companion = None
    try:
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(base / 'system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        if not finder:
            image.remove('/System Folder/Finder')
            image.import_macbinary(output / 'packages/Finder.bin', '/System Folder')
        if without_driver:
            image.remove('/System Folder/Drivers/StuffIt Filesystem')
            image.mkdir('/No StuffIt Driver')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                        str(dist / 'system7.qcow2')], check=True)
        shutil.copyfile(base / 'bootrom.bin', dist / 'bootrom.bin')
        host = vm.build_dir() / 'host/debug'
        exports = []
        for directory in ['writable', 'readonly']:
            folder = output / directory
            subprocess.run([str(host / 'risc-forks'), 'init', str(folder), directory], check=True)
            source = output / 'Examples.sit'
            shutil.copyfile(vm.ROOT / 'System7/crates/mac-sit/tests/fixtures/nested-v1.sit', source)
            if finder_error:
                # Raw host files, rather than only pre-imported fork containers.
                # The failed open must not poison selection or the next open.
                damaged = bytearray(source.read_bytes())
                damaged[14] = 99
                (folder / 'A.sit').write_bytes(damaged)
            else:
                subprocess.run([str(host / 'risc-forks'), 'import', str(folder), str(source)], check=True)
            exports.append(folder)
        companion_log = (output / 'host.log').open('w')
        companion = subprocess.Popen([str(host / 'risc-host'), '--export', f'Writable={exports[0]}',
                                      '--read-only-export', f'ReadOnly={exports[1]}'],
                                     stdout=subprocess.PIPE, stderr=companion_log, text=True)
        companion_log.close()
        if not select.select([companion.stdout], [], [], 5)[0]:
            raise TimeoutError('private StuffIt host companion did not become ready')
        ready = companion.stdout.readline().strip()
        port = int(ready.rsplit(':', 1)[1])
        vm.DIST = dist
        process, endpoint = vm.launch(output / 'vm', writable=True, host_bridge=port, expected_exports=2)
        termination = 'timeout'
        if finder:
            driver = Driver(endpoint, output / 'vm/serial.log')
            driver.click(600, 96)
            driver.key('o', ('ctrl',))
            driver.wait_folder_ready('writable')
            driver.wait_state(lambda s: s['windows'] == '2')
            # Folder-open acknowledges retained drawing; allow the separate
            # presentation path to reach the framebuffer before capturing it.
            time.sleep(.15)
            vm.qmp('screendump', {'filename': str(output / 'archive-icon.ppm')}, endpoint)
            driver.click(56, 106, double=True)
            if finder_error:
                driver.wait('FINDER_ALERT_TEXT ')
                vm.qmp('screendump', {'filename': str(output / 'archive-error.ppm')}, endpoint)
                driver.click(488, 235)
                driver.wait('FINDER_ALERT_DISMISSED\n')
                identity = driver.state()['front']
                marker = f'FINDER_FOLDER_CHANGED window={identity}\n'
                before_change = driver.log().count(marker)
                shutil.copyfile(output / 'Examples.sit', exports[0] / 'B.sit')
                driver.wait(marker, before_change)
                driver.click(120, 106, double=True)
            driver.wait(' name=B.sit\n' if finder_error else ' name=Examples.sit\n')
            driver.wait_state(lambda s: s['windows'] == '3')
            time.sleep(.15)
            vm.qmp('screendump', {'filename': str(output / 'archive-volume.ppm')}, endpoint)
            termination = 'finder-opened-archive'
        deadline = time.monotonic() + 30
        while not finder and time.monotonic() < deadline:
            text = (output / 'vm/serial.log').read_text(errors='replace')
            if 'STUFFIT_PASS' in text or 'STUFFIT_FAIL' in text:
                termination = 'guest-outcome'
                break
            if process.poll() is not None:
                termination = 'process-exited'
                break
            time.sleep(.02)
        trace_snapshot(output, endpoint, output / 'vm/serial.log', 'complete')
        (output / 'capture.json').write_text(json.dumps({
            'termination': termination, 'serial': str(output / 'vm/serial.log'),
            'original_before': before, 'original_after': {name: digest(base / name) for name in before}
        }, indent=2) + '\n')
    except Exception:
        if process is not None and process.poll() is None:
            vm.qmp('stop', endpoint=endpoint)
            (output / 'failure-registers.txt').write_text(vm.qmp('human-monitor-command',
                {'command-line': 'info registers'}, endpoint))
            trace_snapshot(output, endpoint, output / 'vm/serial.log', 'failure')
        raise
    finally:
        try:
            if process is not None:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
        finally:
            if companion is not None:
                companion.terminate()
                companion.wait(timeout=10)
                companion.stdout.close()
            vm.DIST = base
            for folder in [dist, output / 'vm']:
                for pattern in ['*.raw', '*.qcow2', '*.bin']:
                    for path in folder.glob(pattern):
                        path.unlink()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--without-driver', action='store_true')
    parser.add_argument('--finder', action='store_true')
    parser.add_argument('--finder-error', action='store_true')
    args = parser.parse_args()
    capture(args.output.resolve(), args.without_driver, args.finder or args.finder_error,
            args.finder_error)
