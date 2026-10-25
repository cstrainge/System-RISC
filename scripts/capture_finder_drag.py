#!/usr/bin/env python3
"""Real Finder pointer drags, overlay traces and cold-boot placement evidence.

Rust owns assertions and inspects the final disk through the public Files SDK.
"""
from system7_platform import tablet_axis
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
import capture_system7_event_trace as tracing
import system7_vm as vm
from system7_trace_capture import input_stream, snapshot as trace_snapshot, backbuffer_snapshot
from hfs_b_tool import Image as NativeImage


def capture(output, windows=False):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    base = vm.DIST
    original = packaging.sha(base / 'system7.qcow2')
    stages = []
    process = endpoint = driver = None

    def start():
        nonlocal process, endpoint, driver
        options = {'frontend':'native-headless','qmp_port':4467} if windows else {}
        process, endpoint = vm.launch(output / 'vm', writable=True, preserve=True, **options)
        driver = Driver(endpoint, output / 'vm/serial.log')

    def stop():
        nonlocal process
        if process is not None:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
            process = None

    def snapshot(name):
        driver.move(440, 350)
        time.sleep(.15)
        vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)
        stages.append({'name': name, 'state': driver.state()})

    def trace(name):
        return trace_snapshot(endpoint,driver.serial,output,name)

    def release_move():
        moves = driver.log().count('FINDER_DRAG_TRANSFER_COMMITTED\n')
        ended = driver.log().count('FINDER_DRAG_ENDED moved\n')
        driver.button(False)
        driver.wait('FINDER_DRAG_TRANSFER_COMMITTED\n', moves)
        driver.wait('FINDER_DRAG_ENDED moved\n', ended)
        time.sleep(.4)

    def selection_probe(name, moved=False, offset=(0, 0)):
        # Click different cached items without visiting another window. Rust
        # checks their rendered selection; input acknowledgement alone is not
        # evidence that the Finder actually changed the set.
        points = ([('cursor', (96, 146)), ('panic', (120, 106)),
                   ('quickdraw', (260, 200))] if moved else
                  [('panic', (120, 106)), ('cursor', (56, 106)),
                   ('quickdraw', (184, 106))])
        for item, point in points:
            driver.click(point[0] + offset[0], point[1] + offset[1])
            snapshot(name + '-' + item)
        # The next gesture presses QuickDraw again. End the deliberate click
        # pair's classic double-click interval before starting another drag.
        time.sleep(.55)

    try:
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(vm.DIST / 'system7.qcow2'), str(output / 'before.raw')], check=True)
        # A lawful negotiated move can still fail (e.g. a name collision). Keep
        # that modal cleanup/retry regression separate from unsupported actions,
        # which must now reject before drop without producing an error dialog.
        image = NativeImage(output / 'before.raw')
        image.mkdir('/Applications/Z Conflict')
        image.import_macbinary(base / 'QuickDraw Test.bin', '/Applications/Z Conflict')
        private = output / 'dist'
        private.mkdir()
        shutil.copyfile(base / 'bootrom.bin', private / 'bootrom.bin')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2',
                        str(output / 'before.raw'), str(private / 'system7.qcow2')], check=True)
        vm.DIST = private
        start()
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        applications_window = re.search(
            r'FINDER_FOLDER_OPEN .* window=(\d+) name=Applications', driver.log())[1]
        snapshot('opened')
        for attempt in range(2):
            ended = driver.log().count('FINDER_DRAG_ENDED rejected\n')
            driver.move(184, 106)
            driver.button(True)
            driver.move(120, 106)
            driver.key_state('alt', True)
            driver.button(False)
            driver.key_state('alt', False)
            driver.wait('FINDER_DRAG_ENDED rejected\n', ended)
            snapshot('incompatible-app-' + str(attempt))
            selection_probe('incompatible-app-' + str(attempt))
        # The duplicate already in Z Conflict makes these moves fail only after
        # legal negotiation. The modal alert must not swallow session cleanup.
        for attempt in range(2):
            failed = driver.log().count('AlreadyExists (-11)')
            ended = driver.log().count('FINDER_DRAG_ENDED rejected\n')
            dismissed = driver.log().count('FINDER_ALERT_DISMISSED\n')
            driver.move(184, 106)
            driver.button(True)
            # Wide application captions put Z Conflict in row two. The opened
            # capture records its actual icon at this location.
            driver.move(120, 158)
            driver.button(False)
            driver.wait('AlreadyExists (-11)', failed)
            driver.wait('FINDER_DRAG_ENDED rejected\n', ended)
            snapshot('failed-drop-' + str(attempt))
            if attempt == 0:
                driver.key('ret')
            else:
                driver.click(488, 238)
            driver.wait('FINDER_ALERT_DISMISSED\n', dismissed)
            selection_probe('failed-drop-' + str(attempt))
        snapshot('recovered')
        driver.move(184, 106)
        driver.button(True)
        driver.move(220, 160)
        time.sleep(.15)
        back_before = backbuffer_snapshot(endpoint,driver.serial,output,'tracking-back-before')
        before_state, before_trace = driver.state(), trace('tracking-before')
        submitted=[]
        with input_stream(endpoint) as send:
            started=time.monotonic()
            for i in range(200):
                point=[221+i%12,161+i%9] if i<199 else [235,175]
                remaining=started+i*.008-time.monotonic()
                if remaining>0:time.sleep(remaining)
                send([{'type':'abs','data':{'axis':'x','value':tablet_axis(point[0], 'x')}},
                      {'type':'abs','data':{'axis':'y','value':tablet_axis(point[1], 'y')}}])
                submitted.append({'point':point,'seconds':time.monotonic()-started})
        driver.wait_state(lambda state:state['mouse']=='235,175')
        after_trace, after_state = trace('tracking-after'), driver.state()
        back_after = backbuffer_snapshot(endpoint,driver.serial,output,'tracking-back-after')
        vm.qmp('screendump', {'filename': str(output / 'tracking.ppm')}, endpoint)
        driver.move(220,160)
        release_move()
        snapshot('single-moved')
        # Add Cursor Test without collapsing QuickDraw's selection, then move
        # the group from the already-selected QuickDraw icon.
        driver.key_state('shift', True)
        driver.click(56, 106)
        driver.key_state('shift', False)
        time.sleep(.1)
        driver.move(220, 160)
        driver.button(True)
        driver.move(260, 200)
        time.sleep(.1)
        release_move()
        snapshot('group-moved')
        # Cancellation must retain the group and its previously saved positions.
        cancelled = driver.log().count('FINDER_DRAG_ENDED cancelled\n')
        driver.move(260, 200)
        driver.button(True)
        driver.move(300, 230)
        driver.key('esc')
        driver.wait('FINDER_DRAG_ENDED cancelled\n', cancelled)
        driver.button(False)
        snapshot('cancelled')
        selection_probe('cancelled-group', moved=True)
        rejected = driver.log().count('FINDER_DRAG_ENDED rejected\n')
        driver.move(260, 200)
        driver.button(True)
        driver.move(500, 350)
        driver.button(False)
        driver.wait('FINDER_DRAG_ENDED rejected\n', rejected)
        selection_probe('outside-rejected', moved=True)
        # Moving a fully visible front window preserves its content pixels; it
        # must not require an Update or focus round-trip to repair hit testing.
        ready_marker = 'FINDER_FOLDER_READY window=' + applications_window + '\n'
        move_updates_before = driver.log().count(ready_marker)
        driver.move(240, 52)
        driver.button(True)
        driver.move(280, 92)
        driver.button(False)
        time.sleep(.2)
        selection_probe('moved-window', moved=True, offset=(40, 40))
        move_updates_after = driver.log().count(ready_marker)
        # Releasing a title drag outside the desktop cancels it. The transform
        # must remain at the last committed bounds, with selection still live.
        driver.move(280, 92)
        driver.button(True)
        driver.move(280, 10)
        driver.button(False)
        time.sleep(.2)
        selection_probe('cancelled-window-drag', moved=True, offset=(40, 40))
        (output / 'boot-1.log').write_text(driver.log())
        stop()
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(output / 'vm/system7.qcow2'), str(output / 'after.raw')], check=True)
        start()
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        snapshot('rebooted')
        (output / 'boot-2.log').write_text(driver.log())
        (output / 'capture.json').write_text(json.dumps({
            'original_before': original, 'original_after': packaging.sha(base / 'system7.qcow2'),
            'stages': stages, 'tracking_before': before_state, 'tracking_after': after_state,
            'tracking_trace': tracing.delta(before_trace, after_trace),
            'tracking_back_before':back_before,'tracking_back_after':back_after,
            'tracking_submitted':submitted,
            'launch':json.loads((output/'vm/command.json').read_text()),
            'moved_window_updates_before': move_updates_before,
            'moved_window_updates_after': move_updates_after,
        }, indent=2) + '\n')
    finally:
        stop()
        vm.DIST = base
        for name in ['system7.qcow2', 'bootrom.bin']:
            (output / 'vm' / name).unlink(missing_ok=True)
            (output / 'dist' / name).unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--windows-headless',action='store_true')
    args=parser.parse_args()
    capture(args.output.resolve(),args.windows_headless)
