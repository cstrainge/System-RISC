#!/usr/bin/env python3
"""Transport actual Finder source/public-SDK selection checkpoints; Rust asserts."""
import argparse
import json
from pathlib import Path
import time
import re
import subprocess
import struct
import capture_system7_event_trace as tracing
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm

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
    trace_match=re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=1',driver.log())
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
        # Retain opt-in resident trace and heap words before terminating this
        # private VM. This is diagnostic transport, never an acceptance oracle.
        if process.poll() is None:
            vm.qmp('stop',endpoint=endpoint)
            if trace_match:
                trace=output/'failure.trace.bin'
                vm.qmp('pmemsave',{'val':int(trace_match[1]),'size':int(trace_match[2]),'filename':str(trace)},endpoint)
                (output/'failure.trace.json').write_text(json.dumps(tracing.decode(trace),indent=2)+'\n')
            try:
                symbols=subprocess.run(['llvm-nm','-C',str(original/'System.elf')],capture_output=True,text=True,check=True)
                heap=next(line.split()[0] for line in symbols.stdout.splitlines() if line.endswith('mac_system::kernel::HEAP'))
                words=vm.qmp('human-monitor-command',{'command-line':f'xp /7gx 0x{heap}'},endpoint)
                (output/'failure-heap.txt').write_text(words)
                values=[int(value,16) for value in re.findall(r'0x([0-9a-fA-F]{16})',words)]
                if len(values)==7 and 0 < values[5]-values[4] <= 16*1024*1024:
                    bottom,top=values[4:6]
                    raw=output/'failure-heap.bin'
                    vm.qmp('pmemsave',{'val':bottom,'size':top-bottom,'filename':str(raw)},endpoint)
                    memory=raw.read_bytes()
                    holes=[]
                    address=values[3]
                    seen=set()
                    while bottom<=address<=top-16 and address not in seen:
                        seen.add(address)
                        size,address=struct.unpack_from('<QQ',memory,address-bottom)
                        holes.append(size)
                    (output/'failure-heap.json').write_text(json.dumps({'used':values[1],
                        'free':sum(holes),'largest_free':max(holes,default=0),'holes':len(holes),
                        'invalid_or_cyclic_tail':address!=0},indent=2)+'\n')
            except (OSError,StopIteration,subprocess.CalledProcessError):
                pass
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
