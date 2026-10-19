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
                      'desktop-revealed']:
            wait('RETAINED_UPDATE_STAGE ' + stage + '\n')
            # Leave both actual app threads running across multiple timer
            # slices before capture, including event polling and presentation.
            time.sleep(.15)
            frame = stage + '.ppm'
            vm.qmp('screendump', {'filename': str(output / frame)}, endpoint)
            stages.append({'stage': stage, 'frame': frame})
            vm.qmp('input-send-event', {'events': [
                {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'},
                                         'down': down}} for down in [True, False]]}, endpoint)
        wait('RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=17\n')
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
