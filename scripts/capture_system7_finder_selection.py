#!/usr/bin/env python3
"""Transport actual Finder source/public-SDK selection checkpoints; Rust asserts."""
import argparse
import json
from pathlib import Path
import time
import re
import subprocess
import capture_system7_event_trace as tracing
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm
from system7_diagnostics import capture_failure

STAGES = ['baseline', 'single', 'shift-add', 'selected-noop', 'shift-toggle',
          'marquee-three', 'marquee-shrink', 'marquee-release', 'select-all',
          'outline-stress', 'refreshed', 'multiple-info', 'wrapped', 'wrapped-selected', 'wrapped-noop', 'wrapped-refreshed', 'desktop-single', 'desktop-shift', 'desktop-clear', 'desktop-band-trash',
          'desktop-band-cancel', 'desktop-all', 'desktop-outline', 'desktop-collapse', 'desktop-double-open', 'placement-saved', 'placement-reopened-renamed', 'placement-moved', 'placement-auto-added', 'placement-source-gap']

def capture(output, scroll_only=False):
    output=Path(output).resolve()
    output.mkdir(parents=True,exist_ok=False)
    original=vm.DIST
    before={name:packaging.sha(original/name) for name in ['system7.qcow2','bootrom.bin','System.rsrc']}
    private,binary=packaging.private_disk(output,'finder-selection')
    if scroll_only:
        image=packaging.NativeImage(private/'conformance.raw')
        image.mkdir('/Scroll only')
        subprocess.run(['qemu-img','convert','-f','raw','-O','qcow2',str(private/'conformance.raw'),
                        str(private/'system7.qcow2')],check=True)
    vm.DIST=private
    process,endpoint=vm.launch(output/'vm',writable=True)
    driver=Driver(endpoint,output/'vm/serial.log')
    stages=[]
    trace_match=re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2',driver.log())
    try:
        if not scroll_only:
            driver.wait('EVENT_COORDINATES_MOVE_READY\n')
            driver.move(200, 90)
            driver.button(True)
            driver.move(240, 130)
            driver.button(False)
            driver.key('spc')
        for stage in ([] if scroll_only else STAGES):
            marker='FINDER_SELECTION_STAGE '+stage+'\n'
            deadline=time.monotonic()+(90 if stage in ('outline-stress', 'desktop-outline') else 20)
            while marker not in driver.log():
                if 'FINDER_SELECTION_FAIL' in driver.log() or time.monotonic()>deadline:
                    raise RuntimeError('No checkpoint '+stage+'\n'+driver.log()[-6000:])
                time.sleep(.01)
            frame=stage+'.ppm'
            vm.qmp('screendump',{'filename':str(output/frame)},endpoint)
            row={'name':stage,'frame':frame,'state':driver.state()}
            if trace_match:
                trace=output/(stage+'.trace.bin')
                vm.qmp('stop',endpoint=endpoint)
                try:vm.qmp('pmemsave',{'val':int(trace_match[1]),'size':int(trace_match[2]),'filename':str(trace)},endpoint)
                finally:vm.qmp('cont',endpoint=endpoint)
                row['trace']=tracing.decode(trace)
            stages.append(row)
            (output/'stages.json').write_text(json.dumps(stages,indent=2)+'\n')
            driver.key('spc')
        marker='FINDER_SCROLL_USERSPACE_PASS ' if scroll_only else 'FINDER_SELECTION_USERSPACE_PASS stages=30 outline_steps=4000\n'
        deadline=time.monotonic()+20
        while marker not in driver.log():
            if 'FINDER_SELECTION_FAIL' in driver.log() or time.monotonic()>deadline:
                raise RuntimeError('No completion marker '+marker+'\n'+driver.log()[-6000:])
            time.sleep(.01)
        report={'original_before':before,'original_after':{name:packaging.sha(original/name) for name in before},'stages':stages,'fixture_elf_sha256':packaging.sha(binary),'assertion_owner':'mac-toolbox/tests/finder_selection{,_cases}.rs'}
    except Exception:
        if process.poll() is None:
            capture_failure(output,driver,original)
        raise
    finally:
        try:
            if process.poll() is None:vm.qmp('quit',endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            vm.DIST=original
    report['owned_vm_exited']=process.returncode==0
    (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',required=True)
    parser.add_argument('--scroll-only',action='store_true')
    args=parser.parse_args()
    capture(args.output,args.scroll_only)
