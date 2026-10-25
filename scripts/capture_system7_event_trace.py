#!/usr/bin/env python3
"""Physical-input event scenarios plus paused snapshots of opt-in resident tracing.

Transport/evidence only; SDK Rust tests own regression acceptance. The trace
feature prints its memory layout; no debug trap is added to the application ABI.
"""
import argparse
import json
from pathlib import Path
import re
import struct
import time

from capture_system7_applications_ui import Driver
import capture_system7_conformance as packaging
import system7_vm as vm

KINDS = {0: 'textedit', 1: 'input', 2: 'delivery', 3: 'peek', 4: 'update', 5: 'window',
         6: 'activate', 7: 'render', 8: 'route', 9: 'null', 10: 'poll', 11: 'present',
         12: 'wake', 13: 'wait', 14: 'frame_copy', 15: 'slow_service'}
SERVICES = ['log', 'window', 'draw', 'next_event', 'definition', 'classic', 'geometry',
            'surface', 'quickdraw', 'font', 'region', 'copy_bits', 'world', 'picture',
            'cursor', 'mapping', 'resource', 'icon', 'file', 'control', 'memory',
            'image', 'menu', 'dialog', 'clock', 'scrap', 'events', 'textedit', 'process',
            'wait_event', 'monotonic_clock', 'crash', 'standard_file', 'scroll_box', 'drag', 'positioned_event',
            'inspection', 'theme', 'definition_panic', 'power']


def decode(path):
    values = struct.unpack('<' + 'Q' * (path.stat().st_size // 8), path.read_bytes())
    magic, version, seq, capacity, apps, services = values[:6]
    if magic != 0x5253435954524143 or version != 2:
        raise ValueError('unrecognized resident trace format')
    identities = values[6:6 + apps]
    offset = 6 + apps
    stats = {}
    for slot in range(apps):
        app = identities[slot]
        target = {}
        if slot == 0 or app:
            stats[str(app)] = target
        for service in range(services):
            count, ticks, nested, nested_ticks = values[offset:offset + 4]
            offset += 4
            if count or nested:
                name = SERVICES[service] if service < len(SERVICES) else str(service)
                target[name] = [count, ticks, nested, nested_ticks]
    counters = {}
    for slot in range(apps):
        app = identities[slot]
        target = {KINDS.get(i, str(i)): value
                              for i, value in enumerate(values[offset:offset + 16]) if value}
        if slot == 0 or app:
            counters[str(app)] = target
        offset += 16
    records = []
    for _ in range(capacity):
        row = values[offset:offset + 8]
        offset += 8
        if max(0, seq - capacity) <= row[0] < seq and (row[2] or row[1]):
            records.append(list(row))
    return {'seq': seq, 'services': stats, 'counters': counters,
            'records': sorted(records), 'capacity': capacity}


def delta(before, after):
    counters = {}
    services = {}
    for app in after['services']:
        services[app] = {}
        for name, row in after['services'][app].items():
            old = before['services'].get(app, {}).get(name, [0] * 4)
            diff = [a - b for a, b in zip(row, old)]
            if any(diff):
                services[app][name] = diff
        counters[app] = {name: value - before['counters'].get(app, {}).get(name, 0)
                         for name, value in after['counters'][app].items()
                         if value != before['counters'].get(app, {}).get(name, 0)}
    records = [row for row in after['records'] if row[0] >= before['seq']]
    return {'counters': counters, 'services': services, 'records': records,
            'overwritten': max(0, after['seq'] - before['seq'] - after['capacity']),
            'renders_during_update': sum(row[2] == 7 and row[6] != 0 for row in records),
            'render_seconds': sum(row[5] for row in records if row[2] == 7) / 10000000}


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    hashes = {name: packaging.sha(vm.DIST / name)
              for name in ('system7.qcow2', 'System.rsrc', 'bootrom.bin')}
    options = {'frontend': 'native-headless', 'qmp_port': 4457} if windows else {}
    process, endpoint = vm.launch(output / 'vm', writable=True, **options)
    driver = Driver(endpoint, output / 'vm/serial.log')
    stages = []
    try:
        match = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2', driver.log())
        if not match:
            raise RuntimeError('build with SYSTEM7_EVENT_TRACE=1 cargo xtask image first')
        address, size = map(int, match.groups())

        def snapshot(name):
            path = output / (name + '.bin')
            vm.qmp('stop', endpoint=endpoint)
            try:
                vm.qmp('pmemsave', {'val': address, 'size': size, 'filename': str(path)}, endpoint)
            finally:
                vm.qmp('cont', endpoint=endpoint)
            return decode(path)

        before = snapshot('initial')

        def stage(name, action, settle=.25):
            nonlocal before
            start = len(driver.log())
            state_before = driver.state()
            now = time.monotonic()
            action()
            action_seconds = time.monotonic() - now
            time.sleep(settle)
            after = snapshot(name)
            # A Windows file-backed UART and preempted background app may finish
            # earlier events after the action returns. Require a quiet gap in
            # meaningful records; Null polling only changes cumulative counters.
            deadline = time.monotonic() + 5
            quiescent = False
            while time.monotonic() < deadline:
                time.sleep(.15)
                settled = snapshot(name)
                if settled['seq'] == after['seq']:
                    after = settled
                    quiescent = True
                    break
                after = settled
            evidence = delta(before, after)
            evidence.update(name=name, seconds=time.monotonic() - now,
                            state_before=state_before, state_after=driver.state(),
                            action_seconds=action_seconds,
                            quiescent=quiescent,
                            serial=driver.log()[start:])
            stages.append(evidence)
            before = after
            print(json.dumps({key: evidence[key] for key in
                              ('name', 'seconds', 'renders_during_update', 'render_seconds')}), flush=True)

        def activate(row, app):
            count = driver.log().count(f'PROCESS_ACTIVE app={app}\n')
            active = re.findall(r'PROCESS_ACTIVE app=(\d+)\n', driver.log())
            already_active = bool(active and int(active[-1]) == app)
            driver.menu(618, 93 + row * 18, popup_x=580)
            if not already_active:
                driver.wait(f'PROCESS_ACTIVE app={app}\n', count)

        def key(name, modifiers=()):
            count = driver.log().count('FINDER_TEXTEDIT_STATE ')
            driver.key(name, modifiers)
            driver.wait('FINDER_TEXTEDIT_STATE ', count)

        def burst():
            start = len(driver.log())
            events = [{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': c}, 'down': down}}
                      for c in 'abcdefghijklmnopqrst' for down in (True, False)]
            driver.send(events)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                delta = driver.log()[start:]
                # One physical burst can span several virtio input polls. Wait
                # for all edits to commit, independent of frame batching.
                if sum(map(int, re.findall(r'TEXTEDIT_BATCH keys=(\d+)\n', delta))) >= 20:
                    return
                time.sleep(.002)
            raise TimeoutError('typing batch completion\n' + delta[-3000:])

        # Desktop-ready precedes the asynchronous first folder snapshot and its
        # saved-placement publication/watch delivery. Record that startup work
        # separately before measuring an idle Finder.
        stage('startup-completion', lambda: driver.wait_folder_ready('System RISC'), 1)
        stage('idle-finder', lambda: None, 1)
        stage('open-folder', lambda: (driver.click(40, 95, double=True),
                                      driver.wait(' name=Applications\n')))
        stage('launch-textedit', lambda: (driver.application('Text Edit'),
                                          driver.wait('TEXT_EDIT_READY\n')))
        stage('idle-editor', lambda: None, 1)
        stage('select-all', lambda: key('a', ('ctrl',)))
        stage('single-key', lambda: key('x'), .15)
        stage('single-return', lambda: key('ret'), .15)
        stage('single-navigation', lambda: key('left'), .15)
        stage('typing-burst', burst, .15)
        stage('selection-chord', lambda: key('home', ('ctrl', 'shift')), .15)
        stage('modifier-only', lambda: (driver.key_state('shift', True),
                                       driver.key_state('shift', False)))
        stage('pointer-motion', lambda: [driver.move(x, 220) for x in (230, 260, 300, 340)])
        stage('text-selection', lambda: (driver.move(140, 106), driver.button(True),
                                         driver.move(200, 122), driver.button(False)))
        stage('drag-editor', lambda: (driver.move(350, 80), driver.button(True),
                                      driver.move(365, 90), driver.button(False)))
        stage('grow-editor', lambda: (driver.move(608, 412), driver.button(True),
                                      driver.move(625, 432), driver.button(False)))
        stage('switch-finder', lambda: activate(0, driver.app_id('Finder')))
        stage('same-app-finder', lambda: activate(0, driver.app_id('Finder')))
        stage('launch-quickdraw', lambda: (driver.application('QuickDraw Test'),
                                           driver.wait('QUICKDRAW_TEST_READY\n')))
        stage('idle-three-apps', lambda: None, 1)
        stage('same-app-quickdraw', lambda: activate(1, driver.app_id('QuickDraw Test')))
        stage('switch-editor', lambda: activate(2, driver.app_id('Text Edit')))
        stage('typing-with-backgrounds', burst, .15)
        stage('menu-cancel', lambda: (driver.move(28, 10), driver.button(True),
                                      driver.move(350, 450), driver.button(False)))
        stage('hide-others', lambda: driver.menu(618, 44, popup_x=580))
        stage('show-all', lambda: driver.menu(618, 60, popup_x=580))
        stage('show-all-again', lambda: driver.menu(618, 60, popup_x=580))
        stage('quit-editor', lambda: (driver.discard_text_edit(),
                                      driver.wait(f"PROCESS_EXIT app={driver.app_id('Text Edit')}\n")))
        stage('quit-quickdraw', lambda: (driver.menu(28, 60, popup_x=65),
                                         driver.wait(f"PROCESS_EXIT app={driver.app_id('QuickDraw Test')}\n")))
        stage('relaunch-editor', lambda: (driver.application('Text Edit'),
                                          driver.wait('TEXT_EDIT_READY\n', 1)))
        stage('relaunch-input', lambda: key('z'))
        stage('final-idle', lambda: None, 1)
    finally:
        if process.poll() is None:
            vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)
    report = {'stages': stages, 'original_before': hashes,
              'original_after': {name: packaging.sha(vm.DIST / name) for name in hashes},
              'launch': json.loads((output / 'vm/command.json').read_text()),
              'trace_clock_hz': 10000000, 'records': KINDS,
              'limitations': ['Opt-in instrumentation adds counter/clock overhead.',
                              'Null events are counted without individual ring records.',
                              'Wall time is observational; paused snapshots occur between stages.']}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
