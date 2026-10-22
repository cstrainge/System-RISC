#!/usr/bin/env python3
"""Real Finder pointer drags, overlay traces and cold-boot placement evidence.

Rust owns assertions and inspects the final disk through the public Files SDK.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
import capture_system7_event_trace as tracing
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    original = packaging.sha(vm.DIST / 'system7.qcow2')
    stages = []
    process = endpoint = driver = None

    def start():
        nonlocal process, endpoint, driver
        process, endpoint = vm.launch(output / 'vm', writable=True, preserve=True)
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
        match = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=1', driver.log())
        if not match:
            raise RuntimeError('build the private image with SYSTEM7_EVENT_TRACE=1')
        path = output / (name + '.bin')
        vm.qmp('stop', endpoint=endpoint)
        try:
            vm.qmp('pmemsave', {'val': int(match[1]), 'size': int(match[2]),
                                'filename': str(path)}, endpoint)
        finally:
            vm.qmp('cont', endpoint=endpoint)
        return tracing.decode(path)

    def release_move():
        moves = driver.log().count('FINDER_DRAG_MOVE_COMMITTED\n')
        ended = driver.log().count('FINDER_DRAG_ENDED moved\n')
        driver.button(False)
        driver.wait('FINDER_DRAG_MOVE_COMMITTED\n', moves)
        driver.wait('FINDER_DRAG_ENDED moved\n', ended)
        time.sleep(.4)

    try:
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(vm.DIST / 'system7.qcow2'), str(output / 'before.raw')], check=True)
        start()
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        snapshot('opened')
        # Force an unsupported copy to exercise the same failure path as an
        # external-provider move. The modal alert must not swallow cleanup.
        for attempt in range(2):
            failed = driver.log().count('FINDER_ACTION_FAILED status=-4')
            ended = driver.log().count('FINDER_DRAG_ENDED rejected\n')
            dismissed = driver.log().count('FINDER_ALERT_DISMISSED\n')
            driver.move(184, 106)
            driver.button(True)
            driver.move(212, 146)
            driver.key_state('alt', True)
            driver.button(False)
            driver.key_state('alt', False)
            driver.wait('FINDER_ACTION_FAILED status=-4', failed)
            driver.wait('FINDER_DRAG_ENDED rejected\n', ended)
            snapshot('failed-drop-' + str(attempt))
            driver.key('ret')
            driver.wait('FINDER_ALERT_DISMISSED\n', dismissed)
        snapshot('recovered')
        driver.move(184, 106)
        driver.button(True)
        driver.move(220, 160)
        time.sleep(.15)
        before_state, before_trace = driver.state(), trace('tracking-before')
        # System moves the source outline/cursor without scene recomposition.
        for i in range(40):
            driver.move(220 + i % 7, 160 + i % 5)
        driver.move(220, 160)
        time.sleep(.1)
        after_trace, after_state = trace('tracking-after'), driver.state()
        vm.qmp('screendump', {'filename': str(output / 'tracking.ppm')}, endpoint)
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
            'original_before': original, 'original_after': packaging.sha(vm.DIST / 'system7.qcow2'),
            'stages': stages, 'tracking_before': before_state, 'tracking_after': after_state,
            'tracking_trace': tracing.delta(before_trace, after_trace),
        }, indent=2) + '\n')
    finally:
        stop()
        for name in ['system7.qcow2', 'bootrom.bin']:
            (output / 'vm' / name).unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    capture(parser.parse_args().output.resolve())
