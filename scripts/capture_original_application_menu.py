#!/usr/bin/env python3
"""Capture original System 7 application-menu behavior in a private q800 clone.

Interactive stages remain explicit so menu decisions never depend on opaque sleeps.
This script never addresses the visible reference or System RISC VM endpoints.
"""
import argparse
import json
from pathlib import Path
import sys
import time
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
import capture_events_scrap_reference as provenance
from capture_finder_menus_trash import decode_menus
vm = provenance.vm
ROOT = vm.ROOT
vm.STATE = ROOT / 'working/application-menu-reference'
vm.OUT = ROOT / 'results/application-menu-reference'
vm.ENDPOINT = {'transport': 'unix', 'path': str(vm.STATE / 'qmp.sock')}
DATA = ROOT / 'System7/crates/mac-toolbox/tests/data/application-menu'


def capture(name):
    path = vm.OUT / (name + '.png')
    vm.qmp('screendump', {'filename': str(path), 'format': 'png'}, vm.ENDPOINT)
    return path


def memory(name):
    import hashlib
    import struct
    path = vm.STATE / (name + '-ram.bin')
    vm.qmp('pmemsave', {'val': 0, 'size': 128 * 1024 * 1024,
                      'filename': str(path)}, vm.ENDPOINT)
    b = path.read_bytes()
    h = struct.unpack_from('>I', b, 0xa1c)[0]
    ptr = struct.unpack_from('>I', b, h)[0]
    rows = decode_menus(b, ptr)
    report = {'source': provenance.record(path), 'menus': rows,
              'menu_list_handle': hex(h), 'menu_list_pointer': hex(ptr)}
    (vm.OUT / (name + '-menus.json')).write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(rows, indent=2))


def start():
    try:
        vm.qmp('query-status', endpoint=vm.ENDPOINT)
    except (OSError, ValueError, RuntimeError):
        pass
    else:
        raise RuntimeError('Owned reference already running')
    before = [provenance.record(vm.BASE), provenance.record(ROOT / 'downloads/roms/Quadra-650.ROM')]
    vm.prepare()
    vm.hfs('hmount', vm.STATE / 'reference.raw')
    try:
        vm.hfs('hmkdir', ':Application Menu Probes', check=False)
        for app in ['ZuluProbe', 'AlphaProbe']:
            vm.hfs('hcopy', '-m', ROOT / ('build/application-menu-original-68k/' + app + '.bin'), ':Application Menu Probes:')
    finally:
        vm.hfs('humount')
    (vm.STATE / 'originals-before.json').write_text(json.dumps(before, indent=2) + '\n')
    vm.launch()
    vm.wait_for_finder()
    capture('00-finder')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['start', 'capture', 'point', 'click', 'down', 'up', 'key', 'menus', 'stop'])
    p.add_argument('values', nargs='*')
    a = p.parse_args()
    v = a.values
    if a.action == 'start': start()
    elif a.action == 'capture': print(capture(v[0]))
    elif a.action == 'point': vm.point(*map(int, v))
    elif a.action == 'click': vm.click(*map(int, v[:2]), twice=len(v) > 2)
    elif a.action == 'down': vm.monitor('mouse_button 1'); time.sleep(.25)
    elif a.action == 'up': vm.monitor('mouse_button 0'); time.sleep(.5)
    elif a.action == 'key': vm.key(v[0])
    elif a.action == 'menus': memory(v[0])
    elif a.action == 'stop':
        vm.qmp('quit', endpoint=vm.ENDPOINT)
        for _ in range(100):
            pid = (vm.STATE / 'pid').read_text().strip()
            stat = Path('/proc') / pid / 'stat'
            if not stat.exists() or stat.read_text().split()[2] == 'Z': break
            time.sleep(.1)
        else: raise RuntimeError('Owned VM did not exit')
        before = json.loads((vm.STATE / 'originals-before.json').read_text())
        after = [provenance.record(vm.BASE), provenance.record(ROOT / 'downloads/roms/Quadra-650.ROM')]
        if before != after: raise RuntimeError('Original media changed')
        report = {'target': 'Original Mac OS 7.6.1 application menu',
                  'originals_before': before, 'originals_after': after,
                  'system': provenance.record(vm.STATE / 'System.bin'),
                  'finder': provenance.record(vm.STATE / 'Finder.bin'),
                  'command': json.loads((vm.STATE / 'command.json').read_text()),
                  'tool_identities': provenance.tools_identity(),
                  'sources': [provenance.record(p) for p in sorted((ROOT / 'System7/crates/mac-toolbox/tests/fixtures/application-menu-original').iterdir())],
                  'binary': [provenance.record(ROOT / ('build/application-menu-original-68k/' + a + '.bin')) for a in ['AlphaProbe', 'ZuluProbe']],
                  'transport': provenance.record(Path(__file__)),
                  'actions': provenance.record(vm.STATE / 'actions.jsonl'),
                  'captures': [provenance.record(p) for p in sorted(vm.OUT.glob('*.png')) if p.name != 'wait-for-finder.png'],
                  'menus': [provenance.record(p) for p in sorted(vm.OUT.glob('*-menus.json'))],
                  'owned_vm_exited': True,
                  'isolation': 'Owned raw clone and unique Unix QMP endpoint; original and visible VMs untouched.'}
        DATA.mkdir(parents=True, exist_ok=True)
        (DATA / 'provenance.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__': main()
