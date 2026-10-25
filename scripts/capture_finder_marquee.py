#!/usr/bin/env python3
"""Physical Finder marquee gestures; paused traces are evidence for SDK tests."""
import argparse
import json
from pathlib import Path
import time

from capture_system7_applications_ui import Driver
from capture_system7_event_trace import delta
from system7_trace_capture import input_stream, snapshot, backbuffer_snapshot
import system7_vm as vm


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    options = {'frontend': 'native-headless', 'qmp_port': 4467} if windows else {}
    process, endpoint = vm.launch(output / 'vm', writable=True, **options)
    driver = Driver(endpoint, output / 'vm/serial.log')
    stages = []
    try:
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        # Let initial placement saves/watch refresh finish before starting a
        # gesture. No motion sample is warmed, discarded, or retried.
        time.sleep(1)
        for name, points in [
            ('empty', [(390 + i % 5, 200 + i % 7) for i in range(200)]),
            ('icons', [(35 + (i % 50) * 7, 200 - (i % 50) * 2) for i in range(200)]),
        ]:
            driver.click(400, 220)
            time.sleep(.6)  # End the click's double-click interval.
            driver.move(400, 220)
            driver.button(True)
            before = snapshot(endpoint, driver.serial, output, name + '-before')
            back_before = backbuffer_snapshot(endpoint, driver.serial, output, name + '-back-before')
            state_before = driver.state()
            with input_stream(endpoint) as send:
                start = time.monotonic()
                for i, (x, y) in enumerate(points):
                    remaining = start + i * .008 - time.monotonic()
                    if remaining > 0:
                        time.sleep(remaining)
                    send([{'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
                          {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}])
            driver.wait_state(lambda state: state['mouse'] == '%s,%s' % points[-1])
            time.sleep(.15)
            after = snapshot(endpoint, driver.serial, output, name + '-after')
            back_after = backbuffer_snapshot(endpoint, driver.serial, output, name + '-back-after')
            vm.qmp('screendump', {'filename': str(output / (name + '.png')), 'format': 'png'}, endpoint)
            stages.append({'name': name, 'trace': delta(before, after),
                           'back_before': back_before, 'back_after': back_after,
                           'state_before': state_before, 'state_after': driver.state(),
                           'points': points})
            driver.key('esc')
            driver.button(False)
    finally:
        if process.poll() is None:
            vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)
    (output / 'capture.json').write_text(json.dumps({
        'stages': stages, 'owned_vm_exited': process.returncode == 0,
        'launch': json.loads((output / 'vm/command.json').read_text()),
    }, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
