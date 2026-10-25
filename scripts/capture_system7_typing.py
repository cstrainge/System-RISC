#!/usr/bin/env python3
"""Collect real resident/Finder typing traces. Rust owns acceptance, not Python."""
from system7_platform import tablet_axis
import argparse
import json
import re
from pathlib import Path
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
import system7_vm as vm


def capture(output, windows_headless=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = packaging.private_disk(output, 'typing-probe', features=('input-trace',))
    vm.DIST = private
    options = {'frontend': 'native-headless', 'qmp_port': 4457} if windows_headless else {}
    process, endpoint = vm.launch(output / 'profile-vm', **options)
    serial = output / 'profile-vm/serial.log'
    driver = Driver(endpoint, serial)

    def wait(marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if marker in text:
                return text
            if 'TYPING_PROFILE_FAIL' in text:
                raise RuntimeError(text[-3000:])
            time.sleep(.002)
        raise TimeoutError('missing ' + marker + '\n' + text[-3000:])

    def close():
        if process.poll() is None:
            vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)

    try:
        wait('TYPING_PROFILE_DONE\n')
    finally:
        close()
    vm.DIST = original
    process, endpoint = vm.launch(output / 'finder-vm', **options)
    serial = output / 'finder-vm/serial.log'
    driver = Driver(endpoint, serial)
    steps = []

    def send(events):
        vm.qmp('input-send-event', {'events': events}, endpoint)

    def key_events(key):
        return [{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': key}, 'down': down}}
                for down in [True, False]]

    def chord(key, modifiers=()):
        def modifier(name, down):
            return {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name}, 'down': down}}
        send([modifier(m, True) for m in modifiers] + key_events(key)
             + [modifier(m, False) for m in reversed(modifiers)])

    def move(x, y):
        send([{'type': 'abs', 'data': {'axis': axis, 'value': value}}
              for axis, value in [('x', tablet_axis(x, 'x')),
                                  ('y', tablet_axis(y, 'y'))]])
        time.sleep(.05)

    def button(down):
        send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}])
        time.sleep(.05)

    def step(name, action, reports=1, state=None):
        start = len(serial.read_text(errors='replace'))
        action()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            delta = serial.read_text(errors='replace')[start:]
            complete = (f'FINDER_TEXTEDIT_STATE length={state[0]} '
                        f'selection={state[1]}..{state[2]}\n' in delta) if state else (
                        len(re.findall(r'FINDER_TEXTEDIT_STATE [^\n]*\n', delta)) >= reports)
            if complete:
                steps.append({'name': name, 'log': delta})
                return
            time.sleep(.002)
        raise TimeoutError(name + '\n' + delta)

    try:
        from capture_system7_applications_ui import open_textedit
        open_textedit(endpoint,serial)
        wait('FINDER_TEXTEDIT_READY\n')
        deadline = time.monotonic() + 30
        while not re.search(r'FINDER_TEXTEDIT_STATE [^\n]*\n', serial.read_text(errors='replace')):
            if time.monotonic() > deadline:
                raise TimeoutError('initial Finder editor state')
            time.sleep(.002)
        step('select-sample', lambda: chord('a', ['ctrl']))
        text = 'abcdefghijklmnopqrst'
        start = time.monotonic()
        step('burst', lambda: send([event for c in text for event in key_events(c)]),
             state=(20, 20, 20))
        burst = time.monotonic() - start
        step('select-burst', lambda: chord('a', ['ctrl']))
        step('copy-all', lambda: chord('c', ['ctrl']))
        step('cut-all', lambda: chord('x', ['ctrl']))
        step('paste-all', lambda: chord('v', ['ctrl']))
        step('select-pasted', lambda: chord('a', ['ctrl']))
        step('multiline', lambda: send([event for c in 'abc\rdef\rghi'
                                       for event in key_events('ret' if c == '\r' else c)]),
             state=(11, 11, 11))
        for name, key, modifiers in [
            ('home', 'home', []),
            ('shift-end', 'end', ['shift']),
            ('shift-home-reverse', 'home', ['shift']),
            ('ctrl-home', 'home', ['ctrl']),
            ('ctrl-shift-end', 'end', ['ctrl', 'shift']),
            ('ctrl-shift-home-reverse', 'home', ['ctrl', 'shift']),
            ('right-one', 'right', []), ('right-two', 'right', []),
            ('shift-down', 'down', ['shift']),
            ('shift-down-again', 'down', ['shift']),
            ('shift-up', 'up', ['shift']),
            ('shift-up-reverse', 'up', ['shift']),
            ('shift-left', 'left', ['shift']),
            ('shift-right-reverse', 'right', ['shift']),
            ('end', 'end', []),
            ('shift-home', 'home', ['shift']),
            ('shift-end-reverse', 'end', ['shift']),
            ('ctrl-end', 'end', ['ctrl']),
            ('shift-home-backward', 'home', ['shift']),
            ('shift-end-backward-reverse', 'end', ['shift']),
            ('shift-home-again', 'home', ['shift']),
            ('ctrl-shift-home', 'home', ['ctrl', 'shift']),
            ('ctrl-shift-end-reverse', 'end', ['ctrl', 'shift']),
        ]:
            step(name, lambda k=key, m=modifiers: chord(k, m))
        vm.qmp('screendump', {'filename': str(output / 'finder.ppm')}, endpoint)
        driver.discard_text_edit()
        wait(f"PROCESS_EXIT app={driver.app_id('Text Edit')}\n")
        report = {'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'fixture_elf_sha256': packaging.sha(binary),
                  'assertion_owner': 'mac-toolbox/tests/typing.rs',
                  'burst': {'characters': len(text), 'seconds': burst}, 'steps': steps,
                  'profile_launch': json.loads((output / 'profile-vm/command.json').read_text()),
                  'finder_launch': json.loads((output / 'finder-vm/command.json').read_text())}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    finally:
        close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
