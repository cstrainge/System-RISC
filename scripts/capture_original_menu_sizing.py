#!/usr/bin/env python3
"""Collect original CalcMenuSize observations in an isolated q800 clone."""
import json
from pathlib import Path
import sys
import time
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
import capture_events_scrap_reference as provenance
vm=provenance.vm
ROOT=vm.ROOT
vm.STATE=ROOT/'working/menu-sizing-reference'
vm.OUT=ROOT/'results/menu-sizing-reference'
vm.ENDPOINT={'transport':'unix','path':str(vm.STATE/'qmp.sock')}

def main():
    before=[provenance.record(vm.BASE),provenance.record(ROOT/'downloads/roms/Quadra-650.ROM')]
    vm.prepare()
    vm.hfs('hmount',vm.STATE/'reference.raw')
    try:
        vm.hfs('hmkdir',':System Folder:Startup Items',check=False)
        vm.hfs('hcopy','-m',ROOT/'build/menu-sizing-reference-68k/MenuSizingProbe.bin',':System Folder:Startup Items:')
    finally: vm.hfs('humount')
    process=vm.launch()
    try:
        vm.wait_for_finder()
        time.sleep(1)
        vm.capture('completed')
    finally:
        try: vm.qmp('quit',endpoint=vm.ENDPOINT)
        finally: process.wait(timeout=10)
    vm.hfs('hmount',vm.STATE/'reference.raw')
    try:
        vm.hfs('hcopy','-t',':System Folder:Startup Items:MenuWidths.jsonl',vm.OUT/'runtime.jsonl')
    finally: vm.hfs('humount')
    rows=[json.loads(line) for line in (vm.OUT/'runtime.jsonl').read_text().splitlines()]
    if rows[-1]!={'complete':True}: raise RuntimeError('incomplete original probe')
    after=[provenance.record(vm.BASE),provenance.record(ROOT/'downloads/roms/Quadra-650.ROM')]
    if before!=after: raise RuntimeError('original media changed')
    data=ROOT/'System7/crates/mac-toolbox/tests/data/menu-sizing'
    data.mkdir(parents=True,exist_ok=True)
    (data/'original.json').write_text(json.dumps(rows[:-1],indent=2)+'\n')
    report={'target':'Original System7.6.1 CalcMenuSize, standard MDEF0',
            'originals_before':before,'originals_after':after,
            'system':provenance.record(vm.STATE/'System.bin'),'finder':provenance.record(vm.STATE/'Finder.bin'),
            'binary':provenance.record(ROOT/'build/menu-sizing-reference-68k/MenuSizingProbe.bin'),
            'source':[provenance.record(p) for p in sorted((ROOT/'System7/crates/mac-toolbox/tests/fixtures/menu-sizing-original').iterdir())],
            'transport':provenance.record(Path(__file__)), 'runtime':provenance.record(vm.OUT/'runtime.jsonl'),
            'command':json.loads((vm.STATE/'command.json').read_text()),
            'tool_identities':provenance.tools_identity(),'owned_vm_exited':process.returncode==0}
    (data/'provenance.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(rows,indent=2))

if __name__=='__main__':main()
