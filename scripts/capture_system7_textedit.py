#!/usr/bin/env python3
"""Collect resident TextEdit frames from a private SDK Finder; Rust asserts them."""
import argparse
import json
from pathlib import Path
import time

import capture_system7_conformance as packaging
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = packaging.private_disk(output, 'textedit-probe')
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    snapshots = []

    def wait(marker, count=1):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if text.count(marker) >= count:
                return
            if process.poll() is not None or 'TEXTEDIT_PROBE_FAIL' in text:
                raise RuntimeError('TextEdit fixture stopped:\n' + text[-4000:])
            time.sleep(.01)
        raise TimeoutError('TextEdit did not acknowledge ' + marker)

    def key(name):
        vm.qmp('input-send-event', {'events': [
            {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name}, 'down': down}}
            for down in [True, False]]}, endpoint)

    try:
        for name, command in [('text', None), ('selection', 's'), ('caret', 'c'),
                              ('scroll', 'd'), ('caret_idle_on', 'i'),
                              ('caret_idle_off', 'o'), ('inactive_selection', 'u')]:
            if command:
                key(command)
            wait('TEXTEDIT_PROBE_' + name.upper() + '\n')
            # Drawing is retained. The following event poll presents the frame.
            time.sleep(.1)
            path = output / (name + '.ppm')
            vm.qmp('screendump', {'filename': str(path)}, endpoint)
            snapshots.append({'name': name, 'frame': path.name,
                              'crop': [100, 100, 228, 196]})
        native_carets = []
        for index, (name, command) in enumerate([
                ('return', 'r'), ('multiple_returns', 'm'), ('typed_after_return', 'n'),
                ('backspace_after_return', 'b'), ('delete_return', 'x'),
                ('empty_return', 'e'), ('wrapped_return', 'w'), ('resize_after_return', 'z')], 1):
            frames = {}
            for phase, command, marker in [('base', command, 'TEXTEDIT_NATIVE_BASE\n'),
                                            ('caret', 'v', 'TEXTEDIT_NATIVE_CARET\n')]:
                key(command)
                wait(marker, index)
                path = output / f'native-{name}-{phase}.ppm'
                vm.qmp('screendump', {'filename': str(path)}, endpoint)
                frames[phase] = path.name
            native_carets.append({'name': name, **frames})
        report = {
            'transport': 'private disk -> real resident RV64 TextEdit -> public SDK',
            'assertion_owner': 'mac-toolbox/tests/textedit_graphical.rs',
            'snapshots': snapshots,
            'native_carets': native_carets,
            'original_before': before,
            'original_after': {name: packaging.sha(original / name) for name in before},
            'fixture_elf_sha256': packaging.sha(binary),
            'launch': json.loads((output / 'vm/command.json').read_text()),
            'serial_sha256': packaging.sha(serial),
        }
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)

    # The second VM runs the actual Finder and its installed MENU resources.
    vm.DIST = original
    process, endpoint = vm.launch(output / 'finder-vm')
    serial = output / 'finder-vm/serial.log'
    steps = []

    def move(x, y):
        vm.qmp('input-send-event', {'events': [
            {'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
            {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}]}, endpoint)
        time.sleep(.05)

    def button(down):
        vm.qmp('input-send-event', {'events': [
            {'type': 'btn', 'data': {'button': 'left', 'down': down}}]}, endpoint)
        time.sleep(.05)

    def command(name):
        vm.qmp('input-send-event', {'events': [
            {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'ctrl'}, 'down': True}}]}, endpoint)
        key(name)
        vm.qmp('input-send-event', {'events': [
            {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'ctrl'}, 'down': False}}]}, endpoint)

    def step(name, action, reports=1, marker='FINDER_TEXTEDIT_STATE '):
        before = serial.read_text(errors='replace')
        action()
        wait(marker, before.count(marker) + reports)
        time.sleep(.05)
        after = serial.read_text(errors='replace')
        steps.append({'name': name, 'log': after[len(before):]})

    try:
        wait('DESKTOP_READY: Finder and native Window Manager')
        move(290, 10)
        button(True)
        move(290, 30)
        button(False)
        wait('FINDER_TEXTEDIT_READY\n')
        wait('FINDER_TEXTEDIT_STATE ')
        vm.qmp('screendump', {'filename': str(output / 'finder-open.ppm')}, endpoint)
        step('select-all', lambda: command('a'))
        step('type-hello', lambda: [key(c) for c in 'hello'], reports=5)
        step('select-hello', lambda: command('a'))
        step('copy', lambda: command('c'))
        step('cut', lambda: command('x'))
        step('paste', lambda: command('v'))
        step('paste-again', lambda: command('v'))
        step('select-ten', lambda: command('a'))
        step('copy-ten', lambda: command('c'))

        def resize():
            move(598, 408)
            button(True)
            move(538, 348)
            button(False)
        step('resize', resize, marker='FINDER_TEXTEDIT_LAYOUT ')
        move(620, 440)
        vm.qmp('screendump', {'filename': str(output / 'finder-resized.ppm')}, endpoint)
        report['finder_steps'] = steps
        report['finder_launch'] = json.loads((output / 'finder-vm/command.json').read_text())
        report['finder_serial_sha256'] = packaging.sha(serial)
        report['original_after'] = {name: packaging.sha(original / name) for name in before}
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
    capture(parser.parse_args().output)
