#!/usr/bin/env python3
"""Transport for real Finder launches and system application-menu interaction.

This file sends physical QMP input and retains serial/frames. Canonical Rust
tests own acceptance decisions. It never changes the visible desktop VM.
"""
import argparse
import json
from pathlib import Path
import time

import capture_system7_conformance as packaging
import system7_vm as vm


class Driver:
    def __init__(self, endpoint, serial):
        self.endpoint = endpoint
        self.serial = Path(serial)

    def log(self):
        return self.serial.read_text(errors='replace')

    def wait(self, marker, previous=0):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            text = self.log()
            if text.count(marker) > previous:
                return text
            if any(failure in text for failure in ['SYSTEM_PANIC:', 'SYSTEM_TRAP:',
                                                   'SYSTEM_ALLOCATION_FAILED:',
                                                   'TEXTEDIT_EXERCISER_ERROR',
                                                   'QUICKDRAW_TEST_ERROR']):
                raise RuntimeError(text[-5000:])
            time.sleep(.01)
        raise TimeoutError('Guest did not acknowledge ' + marker + '\n' + self.log()[-5000:])

    def send(self, events):
        vm.qmp('input-send-event', {'events': events}, self.endpoint)

    def move(self, x, y):
        marker = f' mouse={x},{y} '
        previous = self.log().count(marker)
        states = [line for line in self.log().splitlines() if line.startswith('WM_STATE ')]
        already_there = bool(states and marker in states[-1])
        self.send([{'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
                   {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}])
        if not already_there:
            self.wait(marker, previous)

    def button(self, down):
        self.send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}])
        time.sleep(.06)

    def click(self, x, y, double=False):
        self.move(x, y)
        for _ in range(2 if double else 1):
            self.button(True)
            self.button(False)

    def key(self, name, modifiers=()):
        def event(code, down):
            return {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': code}, 'down': down}}
        events = [event(code, True) for code in modifiers]
        events += [event(name, True), event(name, False)]
        events += [event(code, False) for code in reversed(modifiers)]
        self.send(events)
        time.sleep(.06)

    def key_state(self, name, down):
        self.send([{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name},
                                          'down': down}}])
        time.sleep(.06)

    def menu(self, x, row_y, popup_x=None):
        self.move(x, 10)
        self.button(True)
        self.move(x if popup_x is None else popup_x, row_y)
        self.button(False)


def open_textedit(endpoint, serial):
    """Fresh actual Finder: open Applications, then double-click TextEdit.

    Shared by the existing typing/caret captures after the exerciser became a
    separate application. Returns after the real editor has drawn its window.
    """
    driver = Driver(endpoint, serial)
    driver.click(40, 95, double=True)
    driver.wait('window=2 name=Applications')
    driver.click(184, 106, double=True)
    driver.wait('TEXTEDIT_EXERCISER_READY\n')
    return driver


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    process, endpoint = vm.launch(output / 'vm', writable=True)
    driver = Driver(endpoint, output / 'vm/serial.log')
    snapshots = []

    def frame(name, park=True):
        if park:
            driver.move(16, 444)
        time.sleep(.1)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        snapshots.append({'name': name, 'frame': path.name,
                          'serial_length': len(driver.log().encode())})

    def activate(row, app):
        previous = driver.log().count(f'PROCESS_ACTIVE app={app}\n')
        driver.menu(618, 93 + row * 18, popup_x=580)
        driver.wait(f'PROCESS_ACTIVE app={app}\n', previous)

    def editor_key(name, modifiers=()):
        previous = driver.log().count('FINDER_TEXTEDIT_STATE ')
        driver.key(name, modifiers)
        driver.wait('FINDER_TEXTEDIT_STATE ', previous)

    try:
        frame('finder-initial')
        driver.move(618, 10)
        driver.button(True)
        frame('finder-only-menu')
        driver.button(False)
        driver.click(40, 95, double=True)
        driver.wait('window=2 name=Applications')
        frame('applications-folder')
        driver.click(56, 106)
        driver.menu(52, 44, popup_x=80)  # Actual Finder File > Open resource item.
        driver.wait('QUICKDRAW_TEST_READY\n')
        frame('quickdraw-launched')
        driver.key('2')
        driver.wait('QD_DEMO_READY page=2 transfer\n')
        frame('quickdraw-page-two')
        activate(0, 1)
        frame('finder-restored')
        driver.click(184, 106, double=True)
        driver.wait('TEXTEDIT_EXERCISER_READY\n')
        driver.key('a', ('ctrl',))
        driver.key('a')
        driver.key('b')
        driver.key('c')
        driver.wait('FINDER_TEXTEDIT_STATE length=3 selection=3..3\n')
        frame('textedit-abc')
        driver.move(618, 10)
        driver.button(True)
        frame('three-app-menu')
        driver.button(False)
        driver.menu(618, 44, popup_x=580)  # Hide Others.
        frame('hide-others')
        driver.menu(618, 60, popup_x=580)  # Show All, foreground unchanged.
        frame('show-all')
        # Expose a narrow editor-content strip to the right of QuickDraw. This
        # permits a real background-content click rather than a menu activation.
        driver.move(350, 80)
        driver.button(True)
        driver.move(380, 100)
        driver.button(False)
        editor_key('home', ('ctrl',))
        activate(1, 2)
        frame('background-editor-before-click')
        activate_count = driver.log().count('PROCESS_ACTIVE app=3\n')
        driver.click(610, 120)
        driver.wait('PROCESS_ACTIVE app=3\n', activate_count)
        editor_key('right')
        # Arrow probes expose the actual click result through the editor's
        # ordinary state report. The app emits no special testing-only service.
        frame('first-background-content-click')
        editor_key('home', ('ctrl',))
        frame('first-click-caret-reset')
        driver.click(610, 120)
        editor_key('left')
        frame('second-foreground-content-click')
        editor_key('end', ('ctrl',))
        frame('editor-caret-restored')
        driver.move(300, 66)
        driver.key_state('ctrl', True)
        driver.button(True)
        driver.move(310, 76)
        frame('background-command-drag-held', park=False)
        driver.button(False)
        driver.key_state('ctrl', False)
        frame('background-command-drag-released')
        driver.move(300, 76)
        driver.button(True)
        driver.move(310, 86)
        frame('background-ordinary-drag-held', park=False)
        activate_count = driver.log().count('PROCESS_ACTIVE app=2\n')
        driver.button(False)
        driver.wait('PROCESS_ACTIVE app=2\n', activate_count)
        frame('background-ordinary-drag-released')
        driver.key('3')
        driver.wait('QD_DEMO_READY page=3 regions\n')
        frame('quickdraw-resumed')
        activate(2, 3)
        driver.key('ret')
        driver.key('d')
        driver.wait('FINDER_TEXTEDIT_STATE length=5 selection=5..5\n')
        frame('textedit-resumed')
        driver.menu(28, 60, popup_x=65)  # TextEdit's own File > Quit.
        driver.wait('PROCESS_EXIT app=3\n')
        frame('textedit-quit')
        driver.menu(28, 60, popup_x=65)  # QuickDraw's own File > Quit.
        driver.wait('PROCESS_EXIT app=2\n')
        frame('quickdraw-quit')
        previous = driver.log().count('TEXTEDIT_EXERCISER_READY\n')
        driver.click(184, 106, double=True)
        driver.wait('TEXTEDIT_EXERCISER_READY\n', previous)
        frame('textedit-relaunched')
        previous = driver.log().count('FINDER_TEXTEDIT_STATE length=1 selection=1..1\n')
        driver.key('a', ('ctrl',))
        driver.key('z')
        driver.wait('FINDER_TEXTEDIT_STATE length=1 selection=1..1\n', previous)
        frame('textedit-relaunched-input')
        driver.menu(28, 60, popup_x=65)
        driver.wait('PROCESS_EXIT app=4\n')
        driver.click(200, 36)  # Bring Finder's startup-volume window forward.
        driver.click(104, 95, double=True)
        driver.wait('name=Documents\n')
        frame('finder-responsive-after-exit')
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
    report = {'transport': 'actual Finder and separately packaged applications; physical QMP input',
              'assertion_owner': 'mac-toolbox/tests/application_ui.rs',
              'snapshots': snapshots, 'owned_vm_exited': process.poll() is not None,
              'original_before': before,
              'original_after': {name: packaging.sha(original / name) for name in before},
              'launch': json.loads((output / 'vm/command.json').read_text()),
              'serial_sha256': packaging.sha(output / 'vm/serial.log')}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
