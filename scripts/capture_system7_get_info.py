#!/usr/bin/env python3
"""Transport real Finder Get Info input on a private writable OS disk.

Python packages generated fixtures, injects physical input and retains frames,
serial and media identities. Canonical Rust tests own correctness acceptance.
"""
from hfs_b_tool import Image as NativeImage
import argparse
import binascii
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import time
from PIL import Image
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm
from system7_trace_capture import snapshot as trace_snapshot
from capture_system7_event_trace import delta


def prepare(output):
    private = output / 'dist'
    private.mkdir()
    raw = private / 'fixtures.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(vm.DIST / 'system7.qcow2'), str(raw)], check=True)


    def file(name, content, destination):
        header = bytearray(128)
        encoded = name.encode('mac_roman')
        header[1] = len(encoded)
        header[2:2 + len(encoded)] = encoded
        header[65:69] = b'TEXT'
        header[69:73] = b'ttxt'
        struct.pack_into('>II', header, 83, len(content), 0)
        struct.pack_into('>II', header, 91, 3875209985, 3875209985)
        header[122:124] = bytes([129, 129])
        struct.pack_into('>H', header, 124, binascii.crc_hqx(header[:124], 0))
        path = private / (name.replace(' ', '-') + '.bin')
        path.write_bytes(header + content + bytes((-len(content)) % 128))
        fs.import_macbinary(path, destination)
    fs = NativeImage(raw)
    fs.mkdir('/0 Info Cases')
    fs.mkdir('/0 Info Cases/Plain Folder')
    fs.mkdir('/0 Info Cases/Nested Folder')
    fs.mkdir('/0 Info Cases/Nested Folder/Child Folder')
    file('Plain File', b'Native Get Info fixture\r'.ljust(36, b'!'), '/0 Info Cases')
    file('Leaf File', bytes((index * 17) & 255 for index in range(1234)),
         '/0 Info Cases/Nested Folder')
    file('Deep File', bytes((index * 31) & 255 for index in range(5678)),
         '/0 Info Cases/Nested Folder/Child Folder')
    shutil.copyfile(vm.DIST / 'bootrom.bin', private / 'bootrom.bin')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    return private


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc', 'Finder.rsrc']}
    private = output / 'dist'
    original_frames = vm.ROOT / 'System7/crates/mac-toolbox/tests/data/get-info'
    references = []
    options = {'frontend': 'native-headless', 'qmp_port': 4467} if windows else {}
    observations = []
    launches = []
    process = None
    driver = None
    endpoint = None
    state = output / 'vm'
    cases_window = None

    def start():
        nonlocal process, endpoint, driver
        process, endpoint = vm.launch(state, writable=True, preserve=True, **options)
        driver = Driver(endpoint, state / 'serial.log')
        launches.append(json.loads((state / 'command.json').read_text()))

    def frame(name):
        driver.move(16, 444)
        time.sleep(.12)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        observations.append({'name': name, 'frame': path.name,
                             'state': driver.state(),
                             'boot': len(launches), 'serial_length': len(driver.log().encode())})

    def text(value):
        events = []
        def key(code, down):
            return {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': code}, 'down': down}}
        for character in value:
            code = {' ': 'spc', '\r': 'ret', '-': 'minus'}.get(character, character.lower())
            shift = character.isupper()
            if shift:
                events.append(key('shift', True))
            events.extend([key(code, True), key(code, False)])
            if shift:
                events.append(key('shift', False))
        driver.send(events)
        time.sleep(.15)

    def open_cases():
        nonlocal cases_window
        previous = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('0 Info Cases')
        log = driver.log()
        row = next(line for line in reversed(log.splitlines())
                   if line.startswith('FINDER_FOLDER_OPEN ') and line.endswith('name=0 Info Cases'))
        cases_window = int(re.search(r' window=(\d+) ', row).group(1))
        # Folder creation is logged inside the second click handler, before its
        # final redraw returns. Drain both physical clicks before selecting a
        # child; native Windows rendering can outlast the driver's key delay.
        driver.wait('FINDER_CLICK_HANDLED\n', previous + 1)

    def select_case(x):
        front = int(driver.state()['front'])
        if front != cases_window:
            # Classic inactive content clicks can only activate their window.
            # Activate through the exposed title first, then select its icon.
            driver.click(280, 52)
            driver.wait_state(lambda state: int(state['front']) == cases_window)
        previous = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.click(x, 106)
        driver.wait('FINDER_CLICK_HANDLED\n', previous)

    def open_info(x, name, folder):
        previous = driver.log().count('FINDER_INFO_OPEN ')
        select_case(x)
        driver.key('i', ('ctrl',))
        log = driver.wait('FINDER_INFO_OPEN ', previous)
        rows = [line for line in log.splitlines() if line.startswith('FINDER_INFO_OPEN ')]
        return rows[-1]

    def close_info():
        previous = driver.log().count('FINDER_INFO_CLOSED ')
        driver.key('w', ('ctrl',))
        driver.wait('FINDER_INFO_CLOSED ', previous)

    def rename_redraw(name):
        log = driver.wait(f'name={name}\n')
        # The commit marker precedes Finder's public-SDK browser redraw. Wait
        # for the Cases window's completed draw after this specific commit,
        # including when that draw already completed before reading the log.
        marker = f'FINDER_FOLDER_CHANGED window={cases_window}\n'
        committed = log.rindex('FINDER_INFO_RENAMED ')
        driver.wait(marker, log[:committed].count(marker))

    def dump_disk(name):
        target = output / name
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(state / 'system7.qcow2'), str(target)], check=True)
        return {'path': target.name, 'sha256': packaging.sha(target)}

    def stop(log_name):
        nonlocal process
        if process is not None:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
            shutil.copyfile(state / 'serial.log', output / log_name)
            process = None

    try:
        private = prepare(output)
        for name in ['02-plain-file', '05-lock-on', '07-empty-folder', '08-nested-folder',
                     '16-stationery-enabled']:
            source = original_frames / (name + '.png')
            target = output / ('original-' + name + '.ppm')
            Image.open(source).convert('RGB').save(target)
            references.append({'name': name, 'source': str(source),
                               'source_sha256': packaging.sha(source), 'frame': target.name})
        vm.DIST = private
        start()
        open_cases()
        opened = open_info(120, 'Plain File', False)
        frame('file-empty')
        idle_before = trace_snapshot(endpoint, driver.serial, output, 'info-caret-before')
        time.sleep(1.2)
        idle_after = trace_snapshot(endpoint, driver.serial, output, 'info-caret-after')
        caret_idle = {'app': driver.app_id('Finder'),
                      'window': int(re.search(r' window=(\d+)', opened).group(1)),
                      'trace': delta(idle_before, idle_after)}
        text('native comment\rsecond line')
        frame('file-comment-draft')
        driver.click(22, 293)
        driver.wait('FINDER_INFO_FLAGS locked=1 stationery=0\n')
        frame('file-locked')
        driver.key('a', ('ctrl',))
        text('blocked')
        frame('file-locked-edit-attempt')
        driver.click(153, 293)  # Disabled Stationery must not change state.
        frame('file-locked-stationery-attempt')
        driver.click(22, 293)
        driver.wait('FINDER_INFO_FLAGS locked=0 stationery=0\n')
        driver.click(153, 293)
        driver.wait('FINDER_INFO_FLAGS locked=0 stationery=1\n')
        frame('file-stationery')
        close_info()
        open_info(120, 'Plain File', False)
        frame('file-reopened')
        # Move the existing Info window away from the real folder icons.
        driver.move(100, 36)
        driver.button(True)
        driver.move(420, 90)
        driver.button(False)
        frame('file-moved')
        select_case(120)
        previous = driver.log().count('FINDER_INFO_REUSED ')
        driver.key('i', ('ctrl',))
        log = driver.wait('FINDER_INFO_REUSED ', previous)
        reused = [line for line in log.splitlines() if line.startswith('FINDER_INFO_REUSED ')][-1]
        reused_window = re.search(r' window=(\d+)', reused).group(1)
        driver.wait_state(lambda state: state['front'] == reused_window)
        frame('file-reused')
        driver.click(430, 122)
        driver.key('a', ('ctrl',))
        text('Plain Folder')
        previous = driver.log().count('FINDER_INFO_ERROR ')
        driver.key('ret')
        driver.wait('FINDER_INFO_ERROR ', previous)
        frame('rename-conflict')
        previous = driver.log().count('FINDER_INFO_ERROR ')
        driver.key('w', ('ctrl',))
        driver.wait('FINDER_INFO_ERROR ', previous)
        frame('close-conflict-retained')
        driver.key('a', ('ctrl',))
        text('z file')
        driver.key('ret')
        rename_redraw('z file')
        frame('file-renamed')
        close_info()
        frame('browser-file-renamed')
        totals = driver.log().count('FINDER_INFO_TOTALS ')
        open_info(184, 'Plain Folder', True)
        driver.wait('FINDER_INFO_TOTALS ', totals)
        frame('folder-empty')
        text('folder comment\rsecond line')
        close_info()
        open_info(184, 'Plain Folder', True)
        frame('folder-reopened')
        driver.click(110, 68)
        driver.key('a', ('ctrl',))
        text('z folder')
        driver.key('ret')
        rename_redraw('z folder')
        frame('folder-renamed')
        close_info()
        totals = driver.log().count('FINDER_INFO_TOTALS ')
        open_info(56, 'Nested Folder', True)
        driver.wait('FINDER_INFO_TOTALS ', totals)
        frame('nested-totals')
        close_info()
        stop('boot-1.log')
        first_disk = dump_disk('boot-1.hfs')
        start()
        open_cases()
        open_info(120, 'z file', False)
        frame('cold-file-reopened')
        driver.click(22, 293)
        driver.wait('FINDER_INFO_FLAGS locked=1 stationery=1\n')
        frame('cold-file-locked')
        driver.key('a', ('ctrl',))
        text('blocked')
        close_info()
        open_info(184, 'z folder', True)
        frame('cold-folder-reopened')
        close_info()
        stop('boot-2.log')
        persisted = dump_disk('persisted.hfs')
        report = {'transport': 'private generated writable disk, real Finder SDK, physical virtio input, cold reboot',
                  'assertion_owner': 'mac-toolbox/tests/get_info_ui.rs',
                  'platform': 'windows-native' if windows else 'linux',
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'original_references': references, 'observations': observations,
                  'first_open': opened, 'launches': launches, 'caret_idle': caret_idle,
                  'boot_1_disk': first_disk, 'persisted_disk': persisted,
                  'owned_vm_exited': process is None,
                  'serial_sha256': {name: packaging.sha(output / name)
                                    for name in ['boot-1.log', 'boot-2.log']},
                  'limits': ['APPL memory-requirements panel deferred',
                             'native filename editing intentionally extends original behavior',
                             'no display latency or power-loss atomicity claim',
                             'runtime storage-failure UI not injected; host backend rollback covered']}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
        # Rust still needs the two persisted raw disks for public-SDK acceptance.
        # The remaining generated VM/build media can now be removed safely.
        for path in [state / 'system7.qcow2', state / 'bootrom.bin']:
            path.unlink(missing_ok=True)
        shutil.rmtree(private)
    finally:
        if process is not None and process.poll() is None:
            try:
                vm.qmp('screendump', {'filename': str(output / 'failure.ppm')}, endpoint)
            except (OSError, ValueError, RuntimeError):
                pass
        stop('interrupted.log')
        failed = not (output / 'capture.json').exists()
        media = [state / 'system7.qcow2', state / 'bootrom.bin']
        if failed:
            media.extend([output / 'boot-1.hfs', output / 'persisted.hfs'])
            identities = {str(path.relative_to(output)): packaging.sha(path)
                          for path in media if path.exists()}
            (output / 'failure-media-hashes.json').write_text(json.dumps(identities, indent=2) + '\n')
        for path in media:
            path.unlink(missing_ok=True)
        if private.exists():
            shutil.rmtree(private)
        vm.DIST = original


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
