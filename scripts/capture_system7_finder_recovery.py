#!/usr/bin/env python3
"""Transport real shell failures, managed report actions and owned restart.

Rust guest assertions own identity, stale-object, fresh-state and survivor checks.
This transport only prepares private media, drives physical input and captures it.
"""
import argparse
import json
import re
from pathlib import Path
import subprocess
import time

import capture_system7_conformance as packaging
from capture_system7_processes import child
from capture_system7_applications_ui import Driver
from hfs_b_tool import Image
import system7_vm as vm
from system7_capture import OwnedProcesses


def capture(output, pressure=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    owners = OwnedProcesses()
    private = output / 'dist'
    snapshots = []
    report = {'pressure': pressure, 'snapshots': snapshots,
              'scope': 'Ordinary std Finder diagnostic with automatic System recovery. '
                       'The optional image-span scenario alone does not establish physical exhaustion.'}
    try:
        private, binary = packaging.private_disk(output, 'finder-recovery')
        worker, worker_binary = child(output, 'finder-recovery-worker', 'RecoveryWorker',
            package_arguments=('--image-pressure',) if pressure else ())
        raw = private / 'conformance.raw'
        fs = Image(raw)
        fs.import_macbinary(worker, '/Applications')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2',
                        str(raw), str(private / 'system7.qcow2')], check=True)
        vm.DIST = private
        vm.RAM = '2G'
        process, endpoint = owners.launch(output / 'vm', frontend='headless', harts=4,
                                          writable=True, clipboard=False)
        driver = Driver(endpoint, output / 'vm/serial.log')
        argv = json.loads((output / 'vm/command.json').read_text())
        assert argv[argv.index('-m') + 1] == '2G'
        assert vm.qmp('query-memory-size-summary', endpoint=endpoint)['base-memory'] == 2147483648
        detected = re.findall(r'^BOOT_RAM base=0x[0-9a-fA-F]+ bytes=(\d+) banks=\d+ reservations=\d+$',
                              driver.log(), re.M)
        assert detected and all(int(value) == 2147483648 for value in detected)
        report['ram_bytes'] = 2147483648

        def frame(name):
            driver.move(16, 550)
            path = output / (name + '.ppm')
            vm.qmp('screendump', {'filename': str(path)}, endpoint)
            snapshots.append({'name': name, 'frame': path.name,
                              'serial_length': len(driver.log().encode()),
                              'state': driver.state()})

        driver.wait('FINDER_RECOVERY_WORKER_READY ')
        if pressure:
            # Prepare the genuine surviving actor BEFORE the user triggers
            # the std panic and its automatic System recovery request.
            driver.click(680, 150)
            driver.key('p')
            driver.wait('FINDER_RECOVERY_PRESSURE_READY\n')
        driver.click(150, 150)
        driver.key('p')
        driver.wait('NATIVE_EXIT_RECOVERY_QUEUED ')
        frame('original-report')
        if pressure:
            driver.wait('FINDER_RESTART_FAILED ')
            frame('admission-refused')
            driver.click(50, 100)
            driver.key('c', ('ctrl',))
            driver.wait('CRASH_REPORT_COPIED ')
            driver.click(680, 150)
            driver.key('q')
            driver.wait('FINDER_RECOVERY_WORKER_EXIT\n')
            driver.wait('PROCESS_EXIT app=' + str(driver.app_id('RecoveryWorker')) + ' ')
            # A real refused automatic admission preserves the report's retry.
            driver.click(50, 100)
            driver.key('ret')

        turns = []
        for turn in range(1, 2 if pressure else 3):
            driver.wait('FINDER_RECOVERY_PASS phase=' + str(turn) + ' ')
            driver.wait('FINDER_RECOVERED ', turn - 1)
            phase = re.search(r'^FINDER_RECOVERY_PASS phase=' + str(turn) +
                              r' old=(\d+) new=(\d+) old_thread=(\d+) new_thread=(\d+) ',
                              driver.log(), re.M)
            assert phase
            old, new, old_thread, new_thread = phase.groups()
            driver.wait('PROCESS_EXIT app=' + old + ' ')
            driver.wait('PROCESS_THREAD_RECLAIMED app=' + old + ' thread=' + old_thread + '\n')
            driver.click(50, 100)
            frame('first-retained-report' if turn == 1 else 'second-report')
            copied = driver.log().count('CRASH_REPORT_COPIED ')
            driver.key('c', ('ctrl',))
            driver.wait('CRASH_REPORT_COPIED ', copied)
            dismissed = driver.log().count('CRASH_REPORT_DISMISSED ')
            driver.click(530, 429)
            driver.wait('CRASH_REPORT_DISMISSED ', dismissed)
            driver.click(150, 150)
            driver.key('x')
            driver.wait('FINDER_RECOVERY_INPUT_ACK\n', turn - 1)
            frame('first-replacement' if turn == 1 else 'second-replacement')
            turns.append({'old': old, 'new': new, 'old_thread': old_thread,
                          'new_thread': new_thread, 'actual_old_account_close': True})
            if not pressure and turn == 1:
                driver.click(680, 150)
                driver.key('x')
                driver.wait('FINDER_RECOVERY_WORKER_INPUT_ACK\n')
                driver.click(150, 150)
                driver.key('p')
                driver.wait('NATIVE_EXIT_RECOVERY_QUEUED ', 1)
        report.update(turns=turns, fixture_elf_sha256=packaging.sha(binary),
                      worker_elf_sha256=packaging.sha(worker_binary))
    finally:
        vm.DIST = original
        report['cleanup'] = owners.cleanup(output, [output / 'vm', private])
        report['original_before'] = before
        report['original_after'] = {name: packaging.sha(original / name) for name in before}
        report['owned_vm_exited'] = all(row['reaped'] for row in report['cleanup']['owners'])
        if (output / 'vm/command.json').exists():
            report['launch'] = json.loads((output / 'vm/command.json').read_text())
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--pressure', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.pressure)
