#!/usr/bin/env python3
"""Private real-Finder incident transport. Rust owns acceptance assertions."""
import argparse
import hashlib
import json
from pathlib import Path
import select
import subprocess
import time

from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output, corrupt=False):
    output.mkdir(parents=True, exist_ok=False)
    host = vm.build_dir() / 'host/debug'
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')], cwd=vm.ROOT / 'System7', check=True)
    export = output / 'export'
    subprocess.run([str(host / 'risc-forks'), 'init', str(export), 'Transfer'], check=True)
    if corrupt:
        source = output / 'Damaged'
        source.write_bytes(b'container corruption fixture')
        subprocess.run([str(host / 'risc-forks'), 'import', str(export), str(source)], check=True)
        physical = next(export.glob('f-*'))
        payload = bytearray(physical.read_bytes())
        payload[224] ^= 1
        physical.write_bytes(payload)
    # These fixtures are deliberately not imported or guest-created.
    (export / 'Test').mkdir()
    (export / 'Notes.txt').write_bytes(b'Ordinary host file\n')
    digest = lambda: hashlib.sha256((vm.DIST / 'system7.qcow2').read_bytes()).hexdigest()
    before = digest()
    process = companion = None
    endpoint = None
    try:
        with (output / 'host.log').open('w') as log:
            companion = subprocess.Popen([str(host / 'risc-host'), '--export', f'Transfer={export}'],
                                          stdout=subprocess.PIPE, stderr=log, text=True)
        if not select.select([companion.stdout], [], [], 5)[0]:
            raise TimeoutError('private companion did not start')
        port = int(companion.stdout.readline().strip().rsplit(':', 1)[1])
        process, endpoint = vm.launch(output / 'vm', writable=True, host_bridge=port, expected_exports=1)
        driver = Driver(endpoint, output / 'vm/serial.log')
        driver.click(600, 96, double=True)
        if corrupt:
            driver.wait('FINDER_ALERT_TEXT ')
            # The watch threshold is 500 ms. Capture after that threshold while
            # the modal stays open, then dismiss using its managed OK control.
            driver.move(350, 350)
            time.sleep(.7)
            vm.qmp('screendump', {'filename': str(output / 'error.ppm')}, endpoint)
            driver.click(488, 235)
            driver.wait('FINDER_ALERT_DISMISSED\n')
            driver.move(350, 350)
            time.sleep(.7)
            vm.qmp('screendump', {'filename': str(output / 'dismissed.ppm')}, endpoint)
        else:
            driver.wait(' name=Transfer\n')
            driver.wait_state(lambda s: s['windows'] == '2')
            time.sleep(.15)
            vm.qmp('screendump', {'filename': str(output / 'plain-entries.ppm')}, endpoint)
            # Let the watch establish its initial revision, then change the
            # directory through the host, without sending any guest input.
            time.sleep(.8)
            previous = driver.log().count('FINDER_FOLDER_CHANGED ')
            (export / 'Arrived.txt').write_bytes(b'Arrived while Finder was running\n')
            driver.wait('FINDER_FOLDER_CHANGED ', previous)
            vm.qmp('screendump', {'filename': str(output / 'refreshed.ppm')}, endpoint)
            driver.key('n', ('ctrl',))
            driver.wait('FINDER_NEW_FOLDER ')
            time.sleep(.15)
            vm.qmp('screendump', {'filename': str(output / 'created-folder.ppm')}, endpoint)
        (output / 'capture.json').write_text(json.dumps({
            'original_before': before, 'original_after': digest(), 'corrupt': corrupt,
            'state': driver.state(), 'export': str(export)
        }, indent=2) + '\n')
    finally:
        try:
            if process is not None:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
        finally:
            if companion is not None:
                companion.terminate()
                companion.wait(timeout=10)
                companion.stdout.close()
            for name in ['system7.qcow2', 'bootrom.bin']:
                (output / 'vm' / name).unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--corrupt', action='store_true')
    args = parser.parse_args()
    capture(args.output.resolve(), args.corrupt)
