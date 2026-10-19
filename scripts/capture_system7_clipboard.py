#!/usr/bin/env python3
"""Capture the real Finder clipboard viewer through a private SDK application.

This supplies packaging, real input and frame transport. Rust owns acceptance.
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
    private, binary = packaging.private_disk(output, 'clipboard')
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    snapshots = []

    def wait(marker):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if marker in text:
                return text
            if process.poll() is not None or 'CLIPBOARD_PROBE_FAIL' in text:
                raise RuntimeError('Clipboard fixture stopped:\n' + text[-4000:])
            time.sleep(.01)
        raise TimeoutError('Clipboard transport did not acknowledge ' + marker)

    def frame(name, marker):
        text = wait(marker)
        # A sample is logged after drawing; a subsequent event poll presents it.
        time.sleep(.1)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        snapshots.append({'name': name, 'frame': path.name, 'log': text})

    def key(name):
        vm.qmp('input-send-event', {'events': [
            {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name}, 'down': down}}
            for down in [True, False]]}, endpoint)

    try:
        frame('empty', 'CLIPBOARD_PROBE_EMPTY\n')
        key('t')
        frame('text', 'CLIPBOARD_PROBE_TEXT\n')
        key('e')
        frame('empty-again', 'CLIPBOARD_PROBE_EMPTY_AGAIN\n')
        report = {
            'transport': 'Finder clipboard source -> public SDK -> real RV64 ECALL',
            'assertion_owner': 'mac-toolbox/tests/clipboard.rs',
            'snapshots': snapshots,
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

    # Also enter the command through the actual Finder and its MENU resources.
    vm.DIST = original
    process, endpoint = vm.launch(output / 'finder-vm')
    serial = output / 'finder-vm/serial.log'

    def move(x, y):
        vm.qmp('input-send-event', {'events': [
            {'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
            {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}]}, endpoint)
        time.sleep(.05)

    def button(down):
        vm.qmp('input-send-event', {'events': [
            {'type': 'btn', 'data': {'button': 'left', 'down': down}}]}, endpoint)
        time.sleep(.05)

    try:
        wait('DESKTOP_READY: Finder and native Window Manager')
        move(90, 10)
        button(True)
        move(110, 156)
        button(False)
        move(600, 300)
        frame('finder-empty', 'FINDER_CLIPBOARD_READY\n')
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
