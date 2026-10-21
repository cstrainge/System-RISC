#!/usr/bin/env python3
"""Record public-SDK update checkpoints on a private live RV64 machine.

Python only packages, transports input and captures frames. Canonical Rust
assertions own pixel comparisons and independent application thread contracts.
"""
import argparse
import json
from pathlib import Path
import time

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = packaging.private_disk(output, 'retained-update')
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    driver = Driver(endpoint, serial)
    stages = []

    def wait(marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if 'RETAINED_UPDATE_FAIL' in text or process.poll() is not None:
                raise RuntimeError('Retained-update fixture stopped:\n' + text[-6000:])
            if marker in text:
                return
            time.sleep(.01)
        raise TimeoutError('No checkpoint ' + marker + ':\n'
                           + serial.read_text(errors='replace')[-6000:])

    try:
        for stage in ['committed-red', 'pending-empty', 'pending-half', 'pending-full',
                      'committed-split', 'pending-yellow', 'committed-yellow',
                      'committed-pattern', 'appended-black', 'pattern-occluded',
                      'pattern-revealed', 'desktop-cyan', 'desktop-pending',
                      'desktop-cancelled', 'desktop-magenta', 'desktop-covered',
                      'desktop-revealed', 'mask-pending-cyan', 'mask-committed-cyan',
                      'mask-pending-cancel', 'mask-cancelled', 'cursor-left',
                      'cursor-outside-patch', 'cursor-erased-patch', 'cursor-pending-patch',
                      'cursor-cancelled-patch', 'cursor-restored-patch', 'cursor-pending-right',
                      'cursor-right', 'cursor-omitted', 'scroll-rows-red',
                      'scroll-row-pending', 'scroll-row-committed']:
            wait('RETAINED_UPDATE_STAGE ' + stage + '\n')
            # Leave both actual app threads running across multiple timer
            # slices before capture, including event polling and presentation.
            time.sleep(.15)
            if stage.startswith('cursor-'):
                frames = []
                for label, point in [('left', [380, 400]), ('right', [550, 400])]:
                    driver.move(*point)
                    frame = stage + '-' + label + '.ppm'
                    vm.qmp('screendump', {'filename': str(output / frame)}, endpoint)
                    frames.append({'point': point, 'frame': frame})
                stages.append({'stage': stage, 'cursor_frames': frames})
            else:
                if stage.startswith('scroll-'):
                    driver.move(16, 444)
                frame = stage + '.ppm'
                vm.qmp('screendump', {'filename': str(output / frame)}, endpoint)
                stages.append({'stage': stage, 'frame': frame, 'state': driver.state()})
            vm.qmp('input-send-event', {'events': [
                {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'},
                                         'down': down}} for down in [True, False]]}, endpoint)
        wait('RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=33\n')
        report = {'transport': 'private blessed disk -> live System -> two native SDK app threads',
                  'assertion_owner': 'mac-toolbox/tests/retained_update{,_cases}.rs',
                  'termination': 'guest-outcome',
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'fixture_elf_sha256': packaging.sha(binary),
                  'launch': json.loads((output / 'vm/command.json').read_text()),
                  'serial': 'vm/serial.log', 'stages': stages}
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            vm.DIST = original
    report['owned_vm_exited'] = process.returncode == 0
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
