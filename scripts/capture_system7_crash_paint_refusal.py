#!/usr/bin/env python3
"""Transport private crash-paint checkpoints; canonical Rust owns acceptance."""
import argparse
import json
from pathlib import Path
import time

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
from system7_diagnostics import capture_failure
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['System.bin', 'System.rsrc', 'system7.qcow2', 'bootrom.bin']}
    private, binary = packaging.private_disk(
        output, 'crash-paint-refusal', package_arguments=(original / 'System.bin',),
        replace_system=True)
    process = endpoint = driver = None
    stages = []

    def wait(marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = driver.log()
            if 'CRASH_PAINT_FAIL' in text or process.poll() is not None:
                raise RuntimeError('Crash-paint fixture stopped:\n' + text[-6000:])
            if marker in text:
                return
            time.sleep(.01)
        raise TimeoutError('No checkpoint ' + marker + ':\n' + driver.log()[-6000:])

    def space(down):
        vm.qmp('input-send-event', {'events': [
            {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'},
                                      'down': down}}]}, endpoint)

    try:
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm')
        driver = Driver(endpoint, output / 'vm/serial.log')
        for stage in ['baseline', 'refused', 'after-mask', 'retry']:
            wait('CRASH_PAINT_STAGE ' + stage + '\n')
            frame = stage + '.ppm'
            vm.qmp('screendump', {'filename': str(output / frame)}, endpoint)
            stages.append({'stage': stage, 'frame': frame, 'state': driver.state()})
            (output / 'stages.json').write_text(json.dumps(stages, indent=2) + '\n')
            space(True)
            try:
                wait('CRASH_PAINT_ACK ' + stage + '\n')
            finally:
                space(False)
        wait('CRASH_PAINT_USERSPACE_PASS full_cancel=passed ownership=passed reset_retry=passed\n')
        report = {'transport': 'private mutated System label -> native public SDK Finder -> ECALL',
                  'assertion_owner': 'mac-toolbox/tests/crash_paint_refusal{,_cases}.rs',
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'fixture_elf_sha256': packaging.sha(binary), 'stages': stages,
                  'mutation': json.loads((output / 'package/mutation.json').read_text())}
    except Exception:
        if process is not None and process.poll() is None and driver is not None:
            capture_failure(output, driver, original)
        raise
    finally:
        try:
            if process is not None and process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            if process is not None:
                process.wait(timeout=10)
            vm.DIST = original
    report['owned_vm_exited'] = process.returncode == 0
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
