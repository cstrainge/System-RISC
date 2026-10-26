#!/usr/bin/env python3
"""Physical Finder power commands on disposable guests; Rust owns acceptance."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import time

from capture_system7_applications_ui import Driver
from system7_platform import display_size
from hfs_b_tool import Image
import system7_vm as vm


def capture(output, windows=False):
    output=Path(output).resolve()
    output.mkdir(parents=True,exist_ok=False)
    original=vm.DIST
    stages=[]
    options={'frontend':'native-headless','qmp_port':4467} if windows else {}
    try:
        for scenario in ['restart','running-app','missing-driver']:
            base=output/scenario
            base.mkdir()
            if scenario=='missing-driver':
                private=base/'dist';private.mkdir()
                raw=private/'image.raw'
                subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(original/'system7.qcow2'),str(raw)],check=True)
                Image(raw).remove('/System Folder/Drivers/Qemu Power Device')
                subprocess.run(['qemu-img','convert','-f','raw','-O','qcow2',str(raw),str(private/'system7.qcow2')],check=True)
                shutil.copyfile(original/'bootrom.bin',private/'bootrom.bin')
                vm.DIST=private
            else:
                vm.DIST=original
            process,endpoint=vm.launch(base/'vm',writable=True,**options)
            driver=Driver(endpoint,base/'vm/serial.log')
            guest_shutdown=False
            try:
                if scenario=='restart':
                    boots=driver.log().count('System RISC Boot ROM v1')
                    ready=driver.log().count('DESKTOP_READY:')
                    driver.menu(255,124,popup_x=270)
                    driver.wait('System RISC Boot ROM v1',boots)
                    driver.wait('DESKTOP_READY:',ready)
                    vm.qmp('screendump',{'filename':str(base/'restarted.png'),'format':'png'},endpoint)
                    driver.menu(255,140,popup_x=270)
                    process.wait(timeout=20);guest_shutdown=True
                elif scenario=='running-app':
                    driver.click(40,95,double=True);driver.wait_folder_ready('Applications')
                    driver.application('Cursor Test');driver.wait('CURSOR_TEST_READY\n')
                    cursor=driver.app_id('Cursor Test');finder=driver.app_id('Finder')
                    active=driver.log().count(f'PROCESS_ACTIVE app={finder}\n')
                    driver.menu(display_size()[0]-22,111,popup_x=display_size()[0]-60);driver.wait(f'PROCESS_ACTIVE app={finder}\n',active)
                    driver.menu(255,140,popup_x=270)
                    driver.wait('Save your documents')
                    vm.qmp('screendump',{'filename':str(base/'refused.png'),'format':'png'},endpoint)
                    driver.key('ret');driver.wait('FINDER_ALERT_DISMISSED\n')
                    active=driver.log().count(f'PROCESS_ACTIVE app={cursor}\n')
                    driver.menu(display_size()[0]-22,93,popup_x=display_size()[0]-60);driver.wait(f'PROCESS_ACTIVE app={cursor}\n',active)
                    driver.key('q',('ctrl',));driver.wait(f'PROCESS_EXIT app={cursor}\n')
                    driver.menu(255,140,popup_x=270)
                    process.wait(timeout=20);guest_shutdown=True
                else:
                    driver.move(255,10);driver.button(True)
                    vm.qmp('screendump',{'filename':str(base/'disabled.png'),'format':'png'},endpoint)
                    driver.move(270,140);driver.button(False)
                    time.sleep(.2)
                stages.append({'scenario':scenario,'guest_shutdown':guest_shutdown,
                               'exit_code':process.poll(),'serial':str((base/'vm/serial.log').relative_to(output)),
                               'launch':json.loads((base/'vm/command.json').read_text())})
            finally:
                if process.poll() is None:vm.qmp('quit',endpoint=endpoint)
                process.wait(timeout=10)
    finally:
        vm.DIST=original
    (output/'capture.json').write_text(json.dumps({'stages':stages},indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',required=True)
    parser.add_argument('--windows-headless',action='store_true')
    args=parser.parse_args();capture(args.output,args.windows_headless)
