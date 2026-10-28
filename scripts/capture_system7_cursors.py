#!/usr/bin/env python3
"""Collect cursor sprites and editable-region states in a private OS guest.

This transport launches the installed Cursor Test through Finder. Rust owns
pixel/behavior acceptance; no input is sent to the user's visible desktop.
"""
from system7_platform import tablet_axis
import argparse
import json
from pathlib import Path
import re
import time

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm
from system7_trace_capture import input_stream, snapshot as trace_snapshot
from capture_system7_event_trace import delta


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    before = {name: packaging.sha(vm.DIST / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc', 'Cursor Test.rsrc']}
    options = {'frontend': 'native-headless', 'qmp_port': 4467} if windows else {}
    process, endpoint = vm.launch(output / 'vm', **options)
    driver = Driver(endpoint, output / 'vm/serial.log')
    frames = []
    pointer_present = None
    active_caret_motion = None

    def backbuffer(name):
        marker = re.search(r'DISPLAY_BUFFERS back=(\d+) front=(\d+) bytes=(\d+)', driver.log())
        if not marker:
            raise RuntimeError('System did not publish its framebuffer diagnostic')
        address, _, length = map(int, marker.groups())
        path = output / (name + '.bin')
        filename = vm.windows_path(path) if windows else str(path)
        vm.qmp('stop', endpoint=endpoint)
        try:
            vm.qmp('pmemsave', {'val': address, 'size': length, 'filename': filename}, endpoint)
        finally:
            vm.qmp('cont', endpoint=endpoint)
        return path.name

    def frame(name, point=None):
        time.sleep(.06)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        frames.append({'name': name, 'frame': path.name, 'point': point,
                       'serial_length': len(driver.log())})

    def action(key, name):
        marker = 'CURSOR_TEST_ACTION kind=' + name + '\n'
        previous = driver.log().count(marker)
        driver.key(key, ('ctrl',))
        driver.wait(marker, previous)

    def region(name, x, y):
        marker = 'CURSOR_TEST_REGION kind=' + name + '\n'
        previous = driver.log().count(marker)
        driver.move(x, y)
        driver.wait(marker, previous)

    try:
        driver.wait('DESKTOP_READY: Finder and native Window Manager')
        driver.click(40, 95, double=True)
        driver.wait(' name=Applications\n')
        driver.application('Cursor Test')
        driver.wait('CURSOR_TEST_READY\n')
        for name, x, y in [('arrow', 142, 126), ('ibeam', 271, 126),
                           ('crosshair', 400, 126), ('plus', 529, 126),
                           ('watch', 142, 200), ('ring', 400, 200),
                           ('ram-disk', 529, 200)]:
            region(name, x, y)
            action('h', 'hide')
            frame(name + '-base', [x, y])
            action('s', 'show')
            frame(name + '-shown', [x, y])
            if name == 'arrow':
                app = driver.app_id('Cursor Test')
                tracing = 'EVENT_TRACE address=' in driver.log()
                start_trace = (trace_snapshot(endpoint, driver.serial, output, 'caret-motion-before')
                               if tracing else None)
                start_state = driver.state()
                submitted = []
                with input_stream(endpoint) as send:
                    start_time = time.monotonic()
                    for index in range(200):
                        # The first move is real; the unique last point fences
                        # the stream instead of matching an earlier cycle.
                        point = [x + 1 + index % 12, y] if index < 199 else [x + 14, y]
                        remaining = start_time + index * .008 - time.monotonic()
                        if remaining > 0:
                            time.sleep(remaining)
                        send([
                            {'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(point[0], 'x')}},
                            {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(point[1], 'y')}}])
                        submitted.append({'point': point, 'seconds': time.monotonic() - start_time})
                driver.wait_state(lambda state: state['mouse'] == f'{point[0]},{point[1]}')
                end_state = driver.state()
                end_trace = (trace_snapshot(endpoint, driver.serial, output, 'caret-motion-after')
                             if tracing else None)
                active_caret_motion = {'app': app, 'state_before': start_state,
                                       'state_after': end_state, 'submitted': submitted,
                                       'trace': delta(start_trace, end_trace) if tracing else None,
                                       'timing_qualified': False,
                                       'limitation': 'Paced physical input and framebuffer observations; '
                                                     'without a trace, per-event latency is unmeasured.'}
                driver.move(x, y)
                # Stop caret animation for this no-scene-damage observation.
                # Input and cursor selection still use the installed SDK app.
                action('l', 'read-only')
                frame('pointer-before', [x, y])
                first = driver.state()
                before_scene = backbuffer('pointer-scene-before')
                driver.move(x + 1, y)
                after = driver.wait_state(lambda state:
                    int(state['presentations']) > int(first['presentations'])
                    and state['mouse'] == f'{x + 1},{y}')
                after_scene = backbuffer('pointer-scene-after')
                frame('pointer-moved', [x + 1, y])
                pointer_present = {'before': before_scene, 'after': after_scene,
                                   'state_before': first, 'state_after': after,
                                   'old_point': [x, y], 'new_point': [x + 1, y]}
                driver.move(x, y)
                action('l', 'editable')
        region('animated-watch', 271, 200)
        action('h', 'hide')
        frame('animation-base', [271, 200])
        action('s', 'show')
        for index in range(10):
            time.sleep(.075)
            frame('animation-' + str(index), [271, 200])
        # Empty portion of the editor avoids text/caret pixels under the sprite.
        region('automatic', 350, 350)
        action('h', 'hide')
        frame('editor-base', [350, 350])
        action('s', 'show')
        frame('editor-ibeam', [350, 350])
        previous = driver.log().count('CURSOR_TEST_EDIT\n')
        driver.key('a')
        driver.wait('CURSOR_TEST_EDIT\n', previous)
        frame('editor-typed-hidden', [350, 350])
        driver.move(351, 350)
        frame('editor-moved', [351, 350])
        action('l', 'read-only')
        frame('editor-read-only', [351, 350])
        action('l', 'editable')
        frame('editor-editable-again', [351, 350])
        action('o', 'obscure')
        action('s', 'show')
        frame('editor-obscured-after-show', [351, 350])
        # Separate input syncs preserve both observed positions, with no wait
        # for a framebuffer between them.
        def axis(x, y):
            return [{'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(x, 'x')}},
                    {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(y, 'y')}}]
        previous = int(driver.state()['input_seq'])
        driver.send(axis(352, 350))
        driver.send(axis(351, 350))
        driver.wait_state(lambda state: int(state['input_seq']) >= previous + 2
                          and state['mouse'] == '351,350')
        frame('editor-returned', [351, 350])
        driver.move(350, 52)
        frame('title-bar-base', [350, 52])
        action('h', 'hide')
        frame('title-bar-hidden', [350, 52])
        action('s', 'show')
        driver.move(350, 350)
        driver.move(104, 10)
        driver.button(True)
        driver.move(350, 350)
        frame('menu-over-editor', [350, 350])
        driver.button(False)
        # A shortcut acknowledgement follows the physical release in the input
        # queue and fences presentation; Show preserves the already-visible
        # cursor and does not cancel menu tracking or change cursor policy.
        action('s', 'show')
        frame('menu-released-editor', [350, 350])
        report = {'assertion_owner': 'mac-toolbox/tests/cursor_graphical.rs',
                  'transport': 'private guest, real Finder app launch and virtio input',
                  'original_before': before,
                  'original_after': {name: packaging.sha(vm.DIST / name) for name in before},
                  'frames': frames,
                  'pointer_present': pointer_present,
                  'active_caret_motion': active_caret_motion,
                  'launch': json.loads((output / 'vm/command.json').read_text()),
                  'serial_sha256': packaging.sha(output / 'vm/serial.log')}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
