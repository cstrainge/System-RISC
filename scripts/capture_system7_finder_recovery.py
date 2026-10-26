#!/usr/bin/env python3
"""Transport real shell failures, managed report actions and owned restart.

Rust guest assertions own identity, stale-object, fresh-state and survivor checks.
This transport only prepares private media, drives physical input and captures it.
"""
import argparse
import json
from pathlib import Path
import subprocess
import time

import capture_system7_conformance as packaging
from capture_system7_processes import child
from capture_system7_applications_ui import Driver
from hfs_b_tool import Image
import system7_vm as vm


def capture(output, pressure=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = packaging.private_disk(output, 'finder-recovery')
    worker, worker_binary = child(output, 'finder-recovery-worker', 'RecoveryWorker',
        package_arguments=('--image-pressure',) if pressure else ())
    raw = private / 'conformance.raw'
    fs = Image(raw)
    fs.import_macbinary(worker, '/Applications')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2',
                    str(raw), str(private / 'system7.qcow2')], check=True)
    vm.DIST = private
    process = None
    snapshots = []
    try:
        process, endpoint = vm.launch(output / 'vm', writable=True)
        driver = Driver(endpoint, output / 'vm/serial.log')

        def frame(name):
            driver.move(16, 550)
            path = output / (name + '.ppm')
            vm.qmp('screendump', {'filename': str(path)}, endpoint)
            snapshots.append({'name': name, 'frame': path.name,
                              'serial_length': len(driver.log().encode()),
                              'state': driver.state()})

        driver.wait('FINDER_RECOVERY_WORKER_READY ')
        driver.wait('CRASH_REPORT_READY ')
        frame('original-report')
        if pressure:
            # The exposed right edge belongs to the surviving worker.
            driver.click(680, 150)
            driver.key('p')
            driver.wait('FINDER_RECOVERY_PRESSURE_READY\n')
            driver.click(50, 100)
            driver.key('ret')
            driver.wait('FINDER_RESTART_FAILED ')
            frame('admission-refused')
            driver.key('c', ('ctrl',))
            driver.wait('CRASH_REPORT_COPIED ')
            driver.click(680, 150)
            driver.key('q')
            driver.wait('FINDER_RECOVERY_WORKER_EXIT\n')
            driver.wait('PROCESS_EXIT app=' + str(driver.app_id('RecoveryWorker')) + '\n')
            driver.click(50, 100)
        driver.key('ret')
        driver.wait('FINDER_RECOVERY_PASS phase=1 ')
        driver.wait('FINDER_RECOVERED ')
        driver.key('x')
        driver.wait('FINDER_RECOVERY_INPUT_ACK\n')
        frame('first-replacement')
        if not pressure:
            driver.click(680, 150)
            driver.key('x')
            driver.wait('FINDER_RECOVERY_WORKER_INPUT_ACK\n')
            driver.click(150, 150)
            previous = driver.log().count('CRASH_REPORT_READY ')
            driver.key('p')
            driver.wait('CRASH_REPORT_READY ', previous)
            frame('second-report')
            # Use the actual managed primary button for this replacement.
            driver.click(530, 429)
            driver.wait('FINDER_RECOVERY_PASS phase=2 ')
            driver.wait('FINDER_RECOVERED ', 1)
            driver.key('x')
            driver.wait('FINDER_RECOVERY_INPUT_ACK\n', 1)
            frame('second-replacement')
        time.sleep(.1)
        report = {'pressure': pressure, 'snapshots': snapshots,
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'fixture_elf_sha256': packaging.sha(binary),
                  'worker_elf_sha256': packaging.sha(worker_binary)}
    finally:
        vm.DIST = original
        if process is not None:
            try:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
            finally:
                process.wait(timeout=10)
        for media in [output / 'vm/system7.qcow2', output / 'vm/bootrom.bin',
                      private / 'system7.qcow2', private / 'bootrom.bin', raw]:
            media.unlink(missing_ok=True)
    report['owned_vm_exited'] = process.poll() is not None
    report['launch'] = json.loads((output / 'vm/command.json').read_text())
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--pressure', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.pressure)
