#!/usr/bin/env python3
"""Collect real public Event Manager input observations on a private RV64 guest.

Python synchronizes input and retains observations. Rust owns every acceptance
decision, including physical key bits, raw OS events and menu-consumed keys.
"""
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
    private, binary = packaging.private_disk(output, 'events-input')
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    snapshots = []

    def text():
        return serial.read_text(errors='replace')

    def state():
        lines = [line for line in text().split('\n')[:-1]
                 if line.startswith('EVENT_INPUT_SAMPLE ')]
        return json.loads(lines[-1].split(' ', 1)[1]) if lines else None

    def wait(predicate):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            current = state()
            if current and predicate(current):
                return current
            if process.poll() is not None or 'EVENT_INPUT_FAIL ' in text():
                raise RuntimeError('Input fixture stopped:\n' + text()[-4000:])
            time.sleep(.01)
        raise TimeoutError('Input transport did not acknowledge packet: ' + str(state()))

    def send(events):
        vm.qmp('input-send-event', {'events': events}, endpoint)

    def key(name, down):
        send([{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name}, 'down': down}}])

    def snapshot(name, predicate):
        snapshots.append({'name': name, 'state': wait(predicate)})

    try:
        snapshot('idle', lambda s: True)
        key('a', True)
        snapshot('a-held', lambda s: int(s['keys'][:2], 16) & 1)
        key('a', False)
        snapshot('a-up', lambda s: s['keys'] == '00' * 16)
        key('shift', True)
        key('a', True)
        snapshot('shift-a-held', lambda s: int(s['keys'][:2], 16) & 1 and s['modifiers'] & 512)
        key('a', False)
        key('shift', False)
        snapshot('shift-a-up', lambda s: s['keys'] == '00' * 16)
        key('ctrl', True)
        key('a', True)
        snapshot('command-a-held', lambda s: int(s['keys'][:2], 16) & 1 and s['modifiers'] & 256)
        wait(lambda _: 'EVENT_INPUT_MENU_COMMAND' in text())
        key('a', False)
        key('ctrl', False)
        snapshot('command-a-up', lambda s: s['keys'] == '00' * 16)
        x, y = 241, 180
        send([{'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
              {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}])
        snapshot('mouse-moved', lambda s: (s['x'], s['y']) == (x, y))
        send([{'type': 'btn', 'data': {'button': 'left', 'down': True}}])
        snapshot('mouse-held', lambda s: not s['modifiers'] & 128)
        wait(lambda _: 'EVENT_INPUT_STILL_DOWN ' in text())
        send([{'type': 'btn', 'data': {'button': 'left', 'down': False}}])
        snapshot('mouse-up', lambda s: s['modifiers'] & 128)
        # Wait for OS queue reporting after the final raw state sample.
        wait(lambda _: any(line.startswith('EVENT_INPUT_OS ') and
                           json.loads(line.split(' ', 1)[1])['kind'] == 2
                           for line in text().splitlines()))
        raw = text()
        report = {
            'transport': 'private graphical disk boot and public SDK RV64 ECALL',
            'assertion_owner': 'mac-toolbox/tests/events.rs',
            'snapshots': snapshots,
            'os_events': [json.loads(line.split(' ', 1)[1]) for line in raw.splitlines()
                          if line.startswith('EVENT_INPUT_OS ')],
            'menu_commands': raw.count('EVENT_INPUT_MENU_COMMAND'),
            'still_down': [json.loads(line.split(' ', 1)[1]) for line in raw.splitlines()
                           if line.startswith('EVENT_INPUT_STILL_DOWN ')],
            'original_before': before,
            'original_after': {name: packaging.sha(original / name) for name in before},
            'fixture_elf_sha256': packaging.sha(binary),
            'launch': json.loads((output / 'vm/command.json').read_text()),
            'serial_sha256': packaging.sha(serial),
        }
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
