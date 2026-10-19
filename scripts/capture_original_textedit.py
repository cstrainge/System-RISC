#!/usr/bin/env python3
"""Collect original mono TextEdit observations in an owned q800 clone."""
import json
from pathlib import Path
import sys
import time
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
import capture_events_scrap_reference as provenance
vm = provenance.vm
ROOT = vm.ROOT
vm.STATE = ROOT / 'working/textedit-reference'
vm.OUT = ROOT / 'results/textedit-reference'
vm.ENDPOINT = {'transport': 'unix', 'path': str(vm.STATE / 'qmp.sock')}


def main():
    before = [provenance.record(vm.BASE), provenance.record(ROOT / 'downloads/roms/Quadra-650.ROM')]
    vm.prepare()
    vm.hfs('hmount', vm.STATE / 'reference.raw')
    try:
        vm.hfs('hmkdir', ':System Folder:Startup Items', check=False)
        vm.hfs('hcopy', '-m', ROOT / 'build/textedit-original-68k/TextEditProbe.bin', ':System Folder:Startup Items:')
    finally:
        vm.hfs('humount')
    process = vm.launch()
    try:
        vm.wait_for_finder()
        time.sleep(1)
        vm.capture('completed')
    finally:
        try:
            vm.qmp('quit', endpoint=vm.ENDPOINT)
        finally:
            process.wait(timeout=10)
    vm.hfs('hmount', vm.STATE / 'reference.raw')
    try:
        vm.hfs('hcopy', '-t', ':System Folder:Startup Items:TextEdit.jsonl', vm.OUT / 'runtime.jsonl')
    finally:
        vm.hfs('humount')
    rows = [json.loads(line) for line in (vm.OUT / 'runtime.jsonl').read_text().splitlines()]
    if rows[-1] != {'complete': True}:
        raise RuntimeError('Original probe incomplete')
    after = [provenance.record(vm.BASE), provenance.record(ROOT / 'downloads/roms/Quadra-650.ROM')]
    if before != after:
        raise RuntimeError('Original media changed')
    data = ROOT / 'System7/crates/mac-toolbox/tests/data/textedit'
    data.mkdir(parents=True, exist_ok=True)
    (data / 'original.json').write_text(json.dumps(rows[:-1], indent=2) + '\n')
    report = dict(target='Original System7.6.1 mono TextEdit, Chicago0/12/face0',
                  originals_before=before, originals_after=after,
                  system=provenance.record(vm.STATE / 'System.bin'),
                  finder=provenance.record(vm.STATE / 'Finder.bin'),
                  binary=provenance.record(ROOT / 'build/textedit-original-68k/TextEditProbe.bin'),
                  source=[provenance.record(p) for p in sorted((ROOT / 'System7/crates/mac-toolbox/tests/fixtures/textedit-original').iterdir())],
                  transport=provenance.record(Path(__file__)),
                  runtime=provenance.record(vm.OUT / 'runtime.jsonl'),
                  command=json.loads((vm.STATE / 'command.json').read_text()),
                  tool_identities=provenance.tools_identity(), owned_vm_exited=process.returncode == 0,
                  limits=['Mono TextEdit only; no styled text, custom hooks, or invalid pointers.',
                          'TEToScrap/TEFromScrap not provided by pinned Retro68 glue; private TE scrap observed via documented low memory.',
                          'Offscreen one-bit GWorld snapshots; caretTime forced just beyond the documented blink interval before TEIdle, no RGB desktop selection color claim.'],
                  documentation=[{'title': 'Inside Macintosh: Text, TextEdit',
                                  'url': 'https://dev.os9.ca/techpubs/mac/pdf/Text/TextEdit.pdf',
                                  'pages': ['2-86', '2-88', '2-99', '2-100', '2-108']}])
    (data / 'provenance.json').write_text(json.dumps(report, indent=2) + '\n')
    print('Captured', len(rows) - 1, 'original mono TextEdit observations.')


if __name__ == '__main__':
    main()
