"""Drive real SDK stalls; Rust owns cursor pixels, restoration and repaint acceptance."""
import argparse,json,re,time
from pathlib import Path
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm

def capture(output):
    output=Path(output).resolve();output.mkdir(parents=True,exist_ok=False)
    original=vm.DIST
    before={name:packaging.sha(original/name) for name in ['system7.qcow2','System.rsrc','bootrom.bin']}
    private,binary=packaging.private_disk(output,'busy-cursor');vm.DIST=private
    process,endpoint=vm.launch(output/'vm');driver=Driver(endpoint,output/'vm/serial.log');frames=[]
    def wait(stage):
        deadline=time.monotonic()+20
        while time.monotonic()<deadline:
            text=driver.log()
            if 'BUSY_CURSOR_FAIL' in text:raise RuntimeError(text[-6000:])
            if f'BUSY_STAGE {stage}\n' in text:return
            time.sleep(.005)
        raise TimeoutError(stage+'\n'+driver.log()[-6000:])
    def frame(name,point=(350,300)):
        def clock():
            raw=vm.qmp('human-monitor-command',{'command-line':'xp /1gx 0x0200bff8'},endpoint)
            values=re.findall(r'0x([0-9a-fA-F]{16})',raw)
            if len(values)!=1:raise RuntimeError('Cannot read guest monotonic timer: '+raw)
            return int(values[0],16)
        before=clock() if name=='short-work' else None
        path=output/(name+'.ppm');vm.qmp('screendump',{'filename':str(path)},endpoint)
        after=clock() if name=='short-work' else None
        frames.append({'name':name,'frame':path.name,'point':point,'state':driver.state(),
                       'clock_before':before,'clock_after':after})
    def begin(stage):
        # The stage marker is the acknowledgement. Driver.key's generic delay
        # plus a host sleep can move an early screenshot past the stall limit.
        driver.send([{'type':'key','data':{'key':{'type':'qcode','data':'spc'},'down':down}}
                     for down in (True,False)])
        wait(stage)
    try:
        wait('idle');driver.move(350,300);time.sleep(.9);frame('idle')
        begin('spin');frame('short-work')
        time.sleep(.5)
        for i in range(6):frame('spin-'+str(i));time.sleep(.12)
        wait('recovered');frame('recovered')
        begin('peeking');time.sleep(.7);frame('peeking');wait('peek-recovered')
        begin('traps');time.sleep(.7)
        for i in range(3):frame('traps-'+str(i));time.sleep(.12)
        wait('trap-recovered');frame('trap-recovered')
        begin('hidden');time.sleep(.7)
        for i in range(3):frame('hidden-'+str(i));time.sleep(.12)
        wait('hidden-recovered');frame('hidden-recovered')
        begin('switch-ready');driver.wait('QD_DEMO_READY page=1 shapes\n');time.sleep(.2);frame('switch-base')
        begin('switch-spin');time.sleep(.7);frame('switch-busy')
        previous=driver.log().count('PROCESS_ACTIVE app=2\n')
        driver.menu(618,111,popup_x=580);driver.wait('PROCESS_ACTIVE app=2\n',previous)
        driver.move(16,444);time.sleep(.8);frame('other-app',(16,444))
        driver.wait('BUSY_CURSOR_USERSPACE_PASS\n');frame('background-recovered',(16,444))
        report={'frames':frames,'original_before':before,'original_after':{name:packaging.sha(original/name) for name in before},'fixture_sha256':packaging.sha(binary)}
    finally:
        if process.poll() is None:vm.qmp('quit',endpoint=endpoint)
        process.wait(timeout=10);vm.DIST=original
        for path in [private/'conformance.raw',private/'system7.qcow2',private/'bootrom.bin',output/'vm/system7.qcow2',output/'vm/bootrom.bin']:path.unlink(missing_ok=True)
    report['owned_vm_exited']=process.returncode==0
    (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True);capture(parser.parse_args().output)
