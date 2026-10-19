#!/usr/bin/env python3
"""Collect deliberate Rust app panic reports in an isolated graphical guest.

The transport drives ordinary Finder, app menus/buttons, the resident report
viewer, and TextEdit. Canonical Rust assertions own report and pixel acceptance.
"""
import argparse
import json
from pathlib import Path
import re
import time

import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    before = {name: packaging.sha(vm.DIST / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc', 'Panic Test.rsrc']}
    options = {'frontend': 'native-headless', 'qmp_port': 4457} if windows else {}
    process, endpoint = vm.launch(output / 'vm', **options)
    driver = Driver(endpoint, output / 'vm/serial.log')
    snapshots = []
    reports = []

    def frame(name):
        driver.move(16, 444)
        time.sleep(.1)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        snapshots.append({'name': name, 'frame': path.name,
                          'serial_length': len(driver.log().encode())})

    def ready(previous):
        # Windows serial files can expose a record before its newline or final
        # fields have flushed. A prefix acknowledges neither complete metadata
        # nor a committed, ready report.
        pattern = r'^CRASH_REPORT_READY app=\d+ window=\d+ bytes=\d+ lines=\d+ page_lines=\d+ flags=\d+\n'
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            lines = re.findall(pattern, driver.log(), re.MULTILINE)
            if len(lines) > previous:
                report = {key: int(value) for key, value in re.findall(r'(\w+)=(\d+)', lines[-1])}
                reports.append(report)
                return report
            time.sleep(.01)
        raise TimeoutError('Complete CRASH_REPORT_READY record did not arrive')

    def launch():
        previous = driver.log().count('PANIC_TEST_READY\n')
        driver.application('Panic Test')
        driver.wait('PANIC_TEST_READY\n', previous)

    def scroll(key):
        previous = driver.log().count('CRASH_REPORT_SCROLL ')
        driver.key(key)
        driver.wait('CRASH_REPORT_SCROLL ', previous)

    def dismissed(report):
        driver.wait(f'CRASH_REPORT_DISMISSED app={report["app"]}\n')
        driver.wait(f'PROCESS_EXIT app={report["app"]}\n')

    try:
        driver.wait('DESKTOP_READY: Finder and native Window Manager')
        driver.click(40, 95, double=True)
        driver.wait('window=2 name=Applications')
        launch()
        frame('panic-app-ready')
        previous = driver.log().count('CRASH_REPORT_READY ')
        # Ordinary app button; short panic captures formatted values and location.
        driver.click(172, 250)
        short = ready(previous)
        frame('short-report')
        driver.key('w', ('ctrl',))
        dismissed(short)
        frame('finder-after-short')
        launch()
        previous = driver.log().count('CRASH_REPORT_READY ')
        driver.key('l', ('ctrl',))
        long = ready(previous)
        frame('long-first-page')
        scroll('end')
        frame('long-last-page')
        scroll('home')
        frame('long-first-page-again')
        previous = driver.log().count('CRASH_REPORT_COPIED ')
        driver.key('c', ('ctrl',))
        driver.wait('CRASH_REPORT_COPIED ', previous)
        # Use the viewer button for the second dismissal.
        driver.click(530, 429)
        dismissed(long)
        frame('finder-after-long')
        driver.application('TextEdit Exerciser')
        driver.wait('TEXTEDIT_EXERCISER_READY\n')
        driver.key('a', ('ctrl',))
        driver.key('v', ('ctrl',))
        driver.wait(f'FINDER_TEXTEDIT_STATE length={long["bytes"]} '
                    f'selection={long["bytes"]}..{long["bytes"]}\n')
        frame('report-pasted-into-textedit')
        driver.key('q', ('ctrl',))
        driver.wait('PROCESS_EXIT app=4\n')
        frame('finder-still-responsive')
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            for name in ['system7.qcow2', 'bootrom.bin']:
                (output / 'vm' / name).unlink(missing_ok=True)
    report = {'transport': 'private guest; installed Panic Test, resident report viewer, and TextEdit',
              'assertion_owner': 'mac-toolbox/tests/panic_graphical.rs',
              'reports': reports, 'snapshots': snapshots,
              'owned_vm_exited': process.poll() is not None,
              'original_before': before,
              'original_after': {name: packaging.sha(vm.DIST / name) for name in before},
              'launch': json.loads((output / 'vm/command.json').read_text()),
              'serial_sha256': packaging.sha(output / 'vm/serial.log')}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
