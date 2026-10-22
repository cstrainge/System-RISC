#!/usr/bin/env python3
"""Physical Finder Get Info evidence on private writable/read-only host mounts.

Rust owns acceptance; this transport records input, serial and screenshots.
"""
import argparse
import hashlib
import json
from pathlib import Path
import select
import subprocess
import time

from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    host = vm.build_dir() / 'host/debug'
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-host', '-p', 'risc-forks',
                    '--target-dir', str(vm.build_dir() / 'host')],
                   cwd=vm.ROOT / 'System7', check=True)
    exports = []
    for label in ['Writable', 'Readonly']:
        export = output / label
        subprocess.run([str(host / 'risc-forks'), 'init', str(export), label], check=True)
        subprocess.run([str(host / 'risc-forks'), 'mkdir', str(export), '/', 'A Folder'],
                       check=True)
        source = output / (label + '.txt')
        source.write_bytes(b'Native file fixture\n')
        subprocess.run([str(host / 'risc-forks'), 'import', str(export), str(source)], check=True)
        # Real host rename is authoritative even for a native fork container.
        (export / source.name).rename(export / 'B File')
        (export / 'C Folder').mkdir()
        (export / 'D File').write_bytes(b'Host file fixture\n')
        exports.append(export)
    digest = lambda: hashlib.sha256((vm.DIST / 'system7.qcow2').read_bytes()).hexdigest()
    before = digest()
    process = companion = endpoint = None
    observations = []
    try:
        with (output / 'host.log').open('w') as log:
            companion = subprocess.Popen([
                str(host / 'risc-host'), '--export', f'Writable={exports[0]}',
                '--read-only-export', f'Readonly={exports[1]}'],
                stdout=subprocess.PIPE, stderr=log, text=True)
        if not select.select([companion.stdout], [], [], 10)[0]:
            raise TimeoutError('private host companion did not start')
        port = int(companion.stdout.readline().strip().rsplit(':', 1)[1])
        process, endpoint = vm.launch(output / 'vm', writable=True,
                                      host_bridge=port, expected_exports=2)
        driver = Driver(endpoint, output / 'vm/serial.log')

        def info(name, mount):
            previous = driver.log().count('FINDER_INFO_OPEN ')
            driver.key('i', ('ctrl',))
            log = driver.wait('FINDER_INFO_OPEN ', previous)
            opened = [line for line in log.splitlines()
                      if line.startswith('FINDER_INFO_OPEN ')][-1]
            driver.move(350, 350)
            time.sleep(.15)
            image = f'{mount}-{len(observations)}.ppm'
            vm.qmp('screendump', {'filename': str(output / image)}, endpoint)
            observations.append({'mount': mount, 'requested': name, 'opened': opened,
                                 'frame': image, 'state': driver.state()})
            previous = driver.log().count('FINDER_INFO_CLOSED ')
            driver.key('w', ('ctrl',))
            driver.wait('FINDER_INFO_CLOSED ', previous)

        for index, mount in enumerate(['Writable', 'Readonly']):
            y = 96 + index * 52
            driver.click(600, y)
            info(mount, mount)
            driver.click(600, y, double=True)
            driver.wait_folder_ready(mount)
            # A fresh root uses the resource-backed 64-pixel icon grid.
            # Provider enumeration order is not a Finder sort guarantee. Visit
            # all four short-caption cells; Rust checks their identities as a set.
            for x in [56, 120, 184, 248]:
                previous = driver.log().count('FINDER_CLICK_HANDLED\n')
                driver.click(x, 106)
                driver.wait('FINDER_CLICK_HANDLED\n', previous)
                info(None, mount)
            # Return to the desktop before selecting the next mount.
            closed = driver.log().count('FINDER_CLOSE_COMMITTED')
            driver.key('w', ('ctrl',))
            driver.wait('FINDER_CLOSE_COMMITTED', closed)
        (output / 'capture.json').write_text(json.dumps({
            'original_before': before, 'original_after': digest(),
            'observations': observations,
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
    parser.add_argument('--output', type=Path, required=True)
    capture(parser.parse_args().output.resolve())
