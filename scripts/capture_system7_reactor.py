#!/usr/bin/env python3
"""Physical Reactor input/frame transport. Rust owns acceptance assertions."""
import argparse
import json
from pathlib import Path
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    original = {name: packaging.sha(vm.DIST / name)
                for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    process = endpoint = None
    snapshots = []
    try:
        process, endpoint = vm.launch(output / 'vm', frontend='headless', writable=True)
        driver = Driver(endpoint, output / 'vm/serial.log')

        def frame(name):
            driver.move(16, 444)
            time.sleep(.1)
            vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)
            snapshots.append({'name': name, 'frame': name + '.ppm',
                              'state': driver.state(), 'serial_length': len(driver.log().encode())})

        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        frame('applications')
        driver.application('Reactor')
        driver.wait('REACTOR_READY ')
        frame('domains')
        driver.click(80, 97)
        frame('selected-first')
        driver.click(80, 97)
        frame('selected-again')
        driver.click(80, 115)
        frame('selected-second')
        driver.key('r', ('ctrl',))
        frame('refreshed')
        driver.move(614, 414)
        driver.button(True)
        driver.move(586, 390)
        driver.button(False)
        frame('resized')
        previous = driver.log().count('PROCESS_EXIT app=')
        driver.key('q', ('ctrl',))
        driver.wait('PROCESS_EXIT app=', previous)
        frame('quit')
        report = {'snapshots': snapshots, 'original_before': original,
                  'original_after': {name: packaging.sha(vm.DIST / name) for name in original},
                  'launch': json.loads((output / 'vm/command.json').read_text())}
    finally:
        if process is not None and process.poll() is None:
            try:
                vm.qmp('quit', endpoint=endpoint)
            finally:
                process.wait(timeout=10)
    report['owned_vm_exited'] = process.poll() is not None
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    capture(parser.parse_args().output.resolve())
