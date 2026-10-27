#!/usr/bin/env python3
"""Collect actual Alert control pixels/trace; canonical Rust decides acceptance."""
import argparse
import json
import re
import time
from pathlib import Path

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
from capture_system7_event_trace import decode, delta
import system7_vm as vm

STAGES = ('base', 'pressed', 'held', 'outside', 'released', 'selected')


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    hashes = {name: packaging.sha(original / name)
              for name in ('system7.qcow2', 'System.rsrc', 'bootrom.bin')}
    process = None
    try:
        private, binary = packaging.private_disk(output, features=('alert-tracking',))
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm')
        serial = output / 'vm/serial.log'
        driver = Driver(endpoint, serial)
        match = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2', driver.log())
        if not match:
            raise RuntimeError('Alert capture requires an event-trace image')
        address, size = map(int, match.groups())
        observations = []
        prior = None
        for name in STAGES:
            driver.wait('ALERT_TRACKING_READY stage=' + name + ' ')
            # The fixture polls after the checkpoint; allow its first retained
            # scene to publish without forcing another application redraw.
            time.sleep(.06)
            state = driver.state()
            line = next(line for line in driver.log().splitlines()
                        if line.startswith('ALERT_TRACKING_READY stage=' + name + ' '))
            bounds = list(map(int, line.split(' bounds=')[1].split(',')))
            path = output / (name + '.bin')
            vm.qmp('stop', endpoint=endpoint)
            try:
                vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)
                vm.qmp('memsave', {'cpu-index': 0, 'val': address, 'size': size, 'filename': str(path)}, endpoint)
            finally:
                vm.qmp('cont', endpoint=endpoint)
            trace = decode(path)
            observations.append({'name': name, 'bounds': bounds, 'state': state,
                                 'delta': None if prior is None else delta(prior, trace)})
            prior = trace
            driver.key('spc')
        driver.wait('ALERT_TRACKING_USERSPACE_PASS cancel=passed release=passed cleanup=passed\n')
        (output / 'capture.json').write_text(json.dumps({
            'original_before': hashes,
            'original_after': {name: packaging.sha(original / name) for name in hashes},
            'fixture_elf_sha256': packaging.sha(binary), 'stages': observations,
            'assertion_owner': 'crates/mac-toolbox/tests/alert_tracking.rs'
        }, indent=2) + '\n')
    finally:
        vm.DIST = original
        if process is not None:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
        for path in (output / 'dist/conformance.raw', output / 'dist/system7.qcow2',
                     output / 'dist/bootrom.bin', output / 'vm/system7.qcow2',
                     output / 'vm/bootrom.bin'):
            path.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
