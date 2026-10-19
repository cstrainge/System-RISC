#!/usr/bin/env python3
"""Collect cursor sprites and editable-region states in a private OS guest.

This transport launches the installed Cursor Test through Finder. Rust owns
pixel/behavior acceptance; no input is sent to the user's visible desktop.
"""
import argparse
import json
from pathlib import Path
import time

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    before = {name: packaging.sha(vm.DIST / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc', 'Cursor Test.rsrc']}
    options = {'frontend': 'native-headless', 'qmp_port': 4457} if windows else {}
    process, endpoint = vm.launch(output / 'vm', **options)
    driver = Driver(endpoint, output / 'vm/serial.log')
    frames = []

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
        driver.wait('window=2 name=Applications')
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
            return [{'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
                    {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}]
        marker = ' mouse=351,350 '
        previous = driver.log().count(marker)
        driver.send(axis(352, 350))
        driver.send(axis(351, 350))
        driver.wait(marker, previous)
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
