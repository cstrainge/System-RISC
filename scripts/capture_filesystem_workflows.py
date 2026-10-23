#!/usr/bin/env python3
"""Physical Finder transfer/document-drop evidence; Rust owns acceptance."""
import argparse
import json
import os
from pathlib import Path
import re
import select
import subprocess
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
from hfs_b_tool import Image
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    host = vm.build_dir() / 'host/debug'
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')],
                   cwd=vm.ROOT / 'System7', check=True)
    exports = []
    for name in ['Writable', 'Readonly']:
        path = output / name
        subprocess.run([str(host / 'risc-forks'), 'init', str(path), name], check=True)
        exports.append(path)
    original = packaging.sha(vm.DIST / 'system7.qcow2')
    process = companion = endpoint = None
    snapshots = []
    try:
        with (output / 'host.log').open('w') as log:
            companion = subprocess.Popen([str(host / 'risc-host'), '--export', f'Writable={exports[0]}',
                                           '--read-only-export', f'Readonly={exports[1]}'],
                                          stdout=subprocess.PIPE, stderr=log, text=True)
        if not select.select([companion.stdout], [], [], 10)[0]:
            raise TimeoutError('private host companion did not start')
        port = int(companion.stdout.readline().strip().rsplit(':', 1)[1])
        process, endpoint = vm.launch(output / 'vm', writable=True, host_bridge=port, expected_exports=2)
        driver = Driver(endpoint, output / 'vm/serial.log')

        def frame(name):
            driver.move(500, 370)
            time.sleep(.1)
            vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)
            snapshots.append({'name': name, 'state': driver.state()})

        def first(name):
            rows = [line for line in driver.log().splitlines()
                    if line.startswith('FINDER_FOLDER_OPEN ') and line.endswith(' name=' + name)]
            identity = rows[-1].split(' window=', 1)[1].split(' ', 1)[0]
            driver.wait('FINDER_FOLDER_LAYOUT window=' + identity + ' ')
            row = [line for line in driver.log().splitlines()
                   if line.startswith('FINDER_FOLDER_LAYOUT window=' + identity + ' ')][-1]
            fields = dict(field.split('=', 1) for field in row.split()[1:])
            icon = list(map(int, fields['first'].split(',')))
            bounds = list(map(int, fields['frame'].split(',')))
            return (bounds[0] + (icon[0] + icon[2]) // 2,
                    bounds[1] + (icon[1] + icon[3]) // 2), bounds, identity

        def drag(source, target, outcome, modifier=None, storage=True):
            ended = driver.log().count('FINDER_DRAG_ENDED ' + outcome + '\n')
            committed = driver.log().count('FINDER_DRAG_TRANSFER_COMMITTED\n')
            driver.move(*source)
            driver.button(True)
            driver.move((source[0] + target[0]) // 2, (source[1] + target[1]) // 2)
            driver.move(*target)
            if modifier:
                driver.key_state(modifier, True)  # Deliberately sampled at drop.
            driver.button(False)
            if modifier:
                driver.key_state(modifier, False)
            if storage:
                driver.wait('FINDER_DRAG_TRANSFER_COMMITTED\n', committed)
            driver.wait('FINDER_DRAG_ENDED ' + outcome + '\n', ended)

        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        drag((56, 106), (600, 148), 'rejected', storage=False)
        driver.click(120, 106)  # Selection must work without a focus round trip.
        frame('readonly-rejection-selection')
        drag((56, 106), (600, 96), 'copied')
        frame('native-to-host')
        driver.click(600, 96, double=True)
        driver.wait_folder_ready('Writable')
        cursor, bounds, identity = first('Writable')
        driver.click(*cursor, double=True)
        driver.wait('CURSOR_TEST_READY')
        exited = driver.log().count('PROCESS_EXIT app=')
        driver.key('q', ('ctrl',))
        driver.wait('PROCESS_EXIT app=', exited)
        frame('external-app-launched-and-quit')
        drag(cursor, (600, 44), 'moved', modifier='ctrl')
        frame('host-to-native-move')
        changed = driver.log().count('FINDER_FOLDER_CHANGED window=' + identity + '\n')
        (exports[0] / 'Note.txt').write_bytes(b'External document\n')
        driver.wait('FINDER_FOLDER_CHANGED window=' + identity + '\n', changed)
        # A newly arriving file uses the vacancy and persists its placement.
        driver.wait_folder_ready('Writable')
        time.sleep(.2)
        note, bounds, identity = first('Writable')
        # Move only the host folder, leaving application icons exposed above it.
        driver.move(bounds[0] + 180, bounds[1] - 10)
        driver.button(True)
        driver.move(bounds[0] + 180, bounds[1] + 182)
        driver.button(False)
        note = (note[0], note[1] + 192)
        frame('external-document')
        drag(note, (200, 400), 'copied', modifier='alt')
        frame('forced-same-volume-copy')
        drag(note, (120, 106), 'rejected', storage=False)
        driver.click(*note)
        frame('incompatible-app-selection')
        time.sleep(.6)  # A new gesture, outside the double-click interval.
        opened = driver.log().count('TEXT_EDIT_DOCUMENT_OPEN\n')
        drag(note, (312, 106), 'copied')
        driver.wait('TEXT_EDIT_DOCUMENT_OPEN\n', opened)
        frame('application-document-drop')
        exited = driver.log().count('PROCESS_EXIT app=')
        driver.key('q', ('ctrl',))
        driver.wait('PROCESS_EXIT app=', exited)
        drag(note, (592, 436), 'moved', modifier='alt')
        frame('trash-ignores-option')
        # Volume icons use eject intent, including readonly mounts. Option
        # must not turn an unmount into a destructive copy/move of the root.
        for index, source in enumerate([(600, 148), (600, 96)]):
            unmounted = driver.log().count('FINDER_DRAG_VOLUME_UNMOUNTED\n')
            drag(source, (592, 436), 'moved', modifier='alt', storage=False)
            driver.wait('FINDER_DRAG_VOLUME_UNMOUNTED\n', unmounted)
            frame('volume-ejected-' + str(index))
        # The startup volume stays usable and refuses Trash eject feedback.
        drag((600, 44), (592, 436), 'rejected', storage=False)
        frame('startup-eject-rejected')
        vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)
        raw = output / 'after.hfsb'
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(output / 'vm/system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        native = image.stat('/Cursor Test')
        image.extract('/Cursor Test', output / 'native-cursor.rsrc', 'resource')
        image.extract('/Applications/Cursor Test', output / 'original-cursor.rsrc', 'resource')
        subprocess.run([str(host / 'risc-forks'), 'export', str(exports[0]), '/',
                        str(output / 'exported')], check=True)
        report = {'original_before': original, 'original_after': packaging.sha(vm.DIST / 'system7.qcow2'),
                  'snapshots': snapshots, 'native_cursor': native,
                  'host_entries': sorted(p.name for p in exports[0].iterdir())}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    except BaseException:
        if process is not None and process.poll() is None:
            vm.qmp('screendump', {'filename': str(output / 'failure.ppm')}, endpoint)
            (output / 'registers.txt').write_text(vm.qmp('human-monitor-command',
                {'command-line': 'info registers'}, endpoint))
        raise
    finally:
        try:
            if process is not None and process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
        finally:
            if companion is not None:
                companion.terminate()
                companion.wait(timeout=10)
                companion.stdout.close()
            for path in [output / 'after.hfsb', output / 'vm/system7.qcow2', output / 'vm/bootrom.bin']:
                path.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    capture(parser.parse_args().output.resolve())
