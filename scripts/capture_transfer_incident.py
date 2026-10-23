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
        physical = export / 'Damaged'
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
    rapid_closed=[]
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
            driver.wait_folder_ready('Transfer')
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
            front=int(driver.state()['front'])
            marker=f'FINDER_FOLDER_CHANGED window={front}\n'
            ready=driver.log().count(marker)
            driver.key('n', ('ctrl',))
            driver.wait('FINDER_NEW_FOLDER ')
            driver.wait(marker,ready)
            vm.qmp('screendump', {'filename': str(output / 'created-folder.ppm')}, endpoint)
            # Exercise actual provider directories, live subscriptions, and
            # physical close boxes. No app-local fake windows or SDK stubs.
            for level in range(3):
                opened=driver.log().count('FINDER_FOLDER_OPEN ')
                driver.key('o',('ctrl',))
                log=driver.wait('FINDER_FOLDER_OPEN ',opened)
                row=next(line for line in reversed(log.splitlines()) if line.startswith('FINDER_FOLDER_OPEN '))
                identity=int(row.split(' window=',1)[1].split(' ',1)[0])
                rapid_closed.append(identity)
                driver.wait(f'FINDER_FOLDER_READY window={identity}\n')
                if level<2:
                    marker=f'FINDER_FOLDER_CHANGED window={identity}\n'
                    ready=driver.log().count(marker)
                    created=driver.log().count('FINDER_NEW_FOLDER ')
                    driver.key('n',('ctrl',))
                    driver.wait('FINDER_NEW_FOLDER ',created)
                    driver.wait(marker,ready)
            vm.qmp('screendump',{'filename':str(output/'rapid-before.ppm')},endpoint)
            # New folder windows use the standard 16px Finder cascade.
            for x,y in [(80,100),(64,84),(48,68)]:
                # QEMU synchronizes each input-send-event batch only once;
                # down/up must be separate physical reports, as on a mouse.
                driver.click(x,y)
            driver.wait_state(lambda state:state['windows']=='2')
            root=next(line for line in driver.log().splitlines() if line.startswith('FINDER_FOLDER_OPEN '))
            root_id=int(root.split(' window=',1)[1].split(' ',1)[0])
            driver.key('n')
            driver.wait_state(lambda state:int(state['front'])==root_id)
            driver.move(350,350)
            time.sleep(.7)
            vm.qmp('screendump',{'filename':str(output/'rapid-close.ppm')},endpoint)
        (output / 'capture.json').write_text(json.dumps({
            'original_before': before, 'original_after': digest(), 'corrupt': corrupt,
            'state': driver.state(), 'export': str(export), 'rapid_closed_windows':rapid_closed
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
