#!/usr/bin/env python3
"""Original Standard File transport; build the Retro68 fixture first."""
import sys,time,json,hashlib
from pathlib import Path
root=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(root/'scripts'))
import capture_finder_reference as vm
vm.STATE=root/'working/standard-file-reference';vm.OUT=root/'results/standard-file-reference';vm.ENDPOINT={'transport':'unix','path':str(vm.STATE/'qmp.sock')}
vm.prepare()
vm.hfs('hmount',vm.STATE/'reference.raw')
try:
 vm.hfs('hmkdir',':System Folder:Startup Items',check=False)
 vm.hfs('hcopy','-m',root/'build/standard-file-original-68k/StandardFileProbe.bin',':System Folder:Startup Items:')
finally:vm.hfs('humount')
p=vm.launch()
try:
 time.sleep(45)
 vm.capture('open-text-filter')
 vm.key('esc');time.sleep(1);vm.capture('save-default')
 vm.key('esc');time.sleep(1)
finally:
 vm.qmp('quit',endpoint=vm.ENDPOINT);p.wait(timeout=10)
vm.hfs('hmount',vm.STATE/'reference.raw')
try: vm.hfs('hcopy','-t',':System Folder:Startup Items:StandardFile.jsonl',vm.OUT/'runtime.jsonl')
finally: vm.hfs('humount')
print((vm.OUT/'runtime.jsonl').read_text())
