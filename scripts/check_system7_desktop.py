#!/usr/bin/env python3
"""Exercise the disk-booted System7 desktop using QMP's real virtio input path."""
from system7_platform import tablet_axis, display_size
import json
from pathlib import Path
import time

import system7_vm as vm
import system7_observer as observer


def check_events(endpoint, serial_path, results):
    from PIL import Image, ImageChops
    results = Path(results)
    records = []

    def state():
        return observer.read(endpoint, serial_path)

    def wait(predicate, description):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            current = state()
            if current and predicate(current):
                return current
            time.sleep(.02)
        raise AssertionError(f'{description}: last guest state {state()}\n'
                             + serial_path.read_text(errors='replace')[-3000:])

    def passed(name, predicate):
        current = wait(predicate, name)
        records.append({'check': name, 'status': 'passed', 'state': current})
        return current

    def send(events):
        vm.qmp('input-send-event', {'events': events}, endpoint)

    def move(x, y):
        send([{'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(x, 'x')}},
              {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(y, 'y')}}])
        wait(lambda s: s['mouse'] == f'{x},{y}', 'absolute pointer motion')

    def button(down):
        before = int(state()['input_seq'])
        send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}])
        wait(lambda s: int(s['input_seq']) > before, 'button transition')

    def click(x, y):
        move(x, y)
        button(True)
        button(False)

    def drag(x, y, nx, ny):
        move(x, y)
        button(True)
        move(nx, ny)
        button(False)

    def key(code, modifiers=()):
        before = int(state()['input_seq'])
        def transition(value, down):
            return {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': value}, 'down': down}}
        events = [transition(mod, True) for mod in modifiers]
        events.extend([transition(code, True), transition(code, False)])
        events.extend(transition(mod, False) for mod in reversed(modifiers))
        send(events)
        wait(lambda s: int(s['input_seq']) >= before + len(events), 'keyboard transition')

    def screenshot(name):
        path = (results / name).resolve()
        vm.qmp('screendump', {'filename': str(path), 'format': 'png'}, endpoint)
        with Image.open(path) as image:
            assert image.size == display_size(), f'unexpected framebuffer dimensions {image.size}'
            return image.convert('RGB')

    def demo_key(code, page):
        marker=f'QD_DEMO_READY page={page}'
        previous=serial_path.read_text(errors='replace').count(marker)
        key(code)
        deadline=time.monotonic()+10
        while serial_path.read_text(errors='replace').count(marker)<=previous:
            assert time.monotonic()<deadline, f'QuickDraw page {page} did not finish after {code}'
            time.sleep(.02)

    def logs():
        return serial_path.read_text(errors='replace')

    def wait_log(marker):
        deadline=time.monotonic()+10
        while marker not in logs():
            assert time.monotonic()<deadline, f'Missing guest marker {marker}\n'+logs()[-4000:]
            time.sleep(.02)

    def window_count(count,front=None):
        return lambda s:s['windows']==str(count) and (front is None or s['front']==str(front))

    try:
        passed('mounted startup volume opens its real HFS-B root folder',
               lambda s:s['windows']=='1' and int(s['front']) != 0 and s['first']=='8,46,412,234' and int(s['updates'])>=1)
        wait_log('FINDER_VOLUME id=')
        root_id = int(state()['front'])
        wait_log(f'FINDER_FOLDER_READY window={root_id}')
        screenshot('finder-startup-volume.png')
        records.append({'check':'Finder enumerates mounted volumes through the resident filesystem',
                        'status':'passed','volumes':[line for line in logs().splitlines() if line.startswith('FINDER_VOLUME ')]})
        from check_wdef_pixels import check_guest_window
        volume_title=next(line.split(' name=',1)[1] for line in logs().splitlines() if line.startswith('FINDER_VOLUME '))
        click(600,50)
        passed('desktop selection preserves front-window z order',window_count(1,root_id))
        # Wait for the application to process selection and commit its scene.
        time.sleep(.1)
        move(16,444)
        screenshot('finder-desktop-focus.png')
        records.append(check_guest_window((results/'finder-desktop-focus.png').resolve(),results/'desktop-inactive-wdef',
                                          bounds=(8,46,412,234),title=volume_title,procedure=8,active=False,grow_clip=(389,173,404,188)))
        click(250,170)
        passed('first content click reactivates the existing front folder',window_count(1,root_id))
        time.sleep(.1)
        move(16,444)
        screenshot('finder-desktop-reactivated.png')
        records.append(check_guest_window((results/'finder-desktop-reactivated.png').resolve(),results/'desktop-reactivated-wdef',
                                          bounds=(8,46,412,234),title=volume_title,procedure=8,active=True,grow_clip=(389,173,404,188)))
        key('a')
        about = passed('A opens the separate About application window',window_count(2))
        about_id = int(about['front'])
        # Keep the separately composited cursor outside all compared WDEF pixels.
        move(16,444)
        screenshot('finder-desktop.png')
        from check_wdef_pixels import check_guest_window
        records.append(check_guest_window((results/'finder-desktop.png').resolve(),results,
                                          bounds=(250,150,550,345)))
        key('q')
        quickdraw = passed('Q opens QuickDraw Exerciser as a separate window',window_count(3))
        quickdraw_id = int(quickdraw['front'])
        wait_log('QD_DEMO_READY page=1')
        screenshot('finder-quickdraw-shapes.png')
        previous=None
        for page,label in [(2,'transfer'),(3,'regions'),(4,'bitmaps'),(5,'fonts'),(6,'offscreen'),(7,'pictures'),(8,'icons'),(9,'resources')]:
            demo_key(str(page),page)
            image=screenshot(f'finder-quickdraw-{label}.png')
            if previous is not None:
                assert ImageChops.difference(previous,image).getbbox() is not None, 'QuickDraw page did not change pixels'
            previous=image
            records.append({'check':f'QuickDraw Exerciser {label} page through application traps',
                            'status':'passed','screenshot':f'results/system7/finder-quickdraw-{label}.png'})
        demo_key('8',8)
        initial_icons=screenshot('finder-quickdraw-icons.png')
        demo_key('right',8)
        next_icons=screenshot('finder-quickdraw-icons-next.png')
        icon_area=(40,100,599,391)
        assert ImageChops.difference(initial_icons.crop(icon_area),next_icons.crop(icon_area)).getbbox() is not None, 'icon browser did not advance'
        demo_key('left',8)
        restored_icons=screenshot('finder-quickdraw-icons-restored.png')
        assert ImageChops.difference(initial_icons.crop(icon_area),restored_icons.crop(icon_area)).getbbox() is None, 'icon browser did not restore its original families'
        records.append({'check':'QuickDraw icon browser advances and restores resource families','status':'passed'})
        drag(593,434,493,374)
        passed('QuickDraw Exerciser resizes and repaints clipped content',window_count(3,quickdraw_id))
        screenshot('finder-quickdraw-resized.png')
        drag(493,374,593,434)
        passed('QuickDraw Exerciser restores its original size',window_count(3,quickdraw_id))
        # Redraw completion follows the resize event and retained scene commit.
        deadline=time.monotonic()+5
        while True:
            restored=screenshot('finder-quickdraw-size-restored.png')
            if ImageChops.difference(initial_icons.crop(icon_area),restored.crop(icon_area)).getbbox() is None:break
            assert time.monotonic()<deadline,'QuickDraw resize roundtrip changed retained content'
            time.sleep(.05)
        records.append({'check':'QuickDraw resize roundtrip restores resource-backed pixels','status':'passed'})
        key('w',('ctrl',))
        passed('QuickDraw window closes and exposes About',window_count(2,about_id))
        key('w',('ctrl',))
        passed('About closes and exposes the real volume browser',window_count(1,root_id))
        move(100,440);move(540,440)
        passed('absolute mouse crosses screen midpoint',lambda s:s['mouse']=='540,440')
        drag(398,35,20,440)
        passed('WDEF zoom release outside its box cancels',lambda s:s['first']=='8,46,412,234')
        click(398,35)
        passed('WDEF zoom expands the folder to desktop limits',lambda s:s['first']=='1,44,638,457')
        screenshot('finder-window-zoom.png')
        click(624,35)
        passed('WDEF zoom restores the user folder bounds',lambda s:s['first']=='8,46,412,234')
        drag(150,33,210,66)
        passed('title drag preserves pointer offset',lambda s:s['first']=='68,79,472,267')
        screenshot('finder-window-drag.png')
        drag(465,260,405,230)
        passed('grow box resizes and repaints the real folder',lambda s:s['first']=='68,79,412,237')
        screenshot('finder-window-resize.png')
        drag(405,230,465,260)
        passed('folder grow roundtrip restores size',lambda s:s['first']=='68,79,472,267')
        drag(210,66,150,33)
        passed('folder drag roundtrip restores position',lambda s:s['first']=='8,46,412,234')
        drag(21,36,20,440)
        passed('close release outside its box cancels',window_count(1,root_id))
        # Actual HFS catalog folders replace the old canned new-window button.
        click(40,92);click(40,92)
        applications=passed('double-click Applications opens its HFS catalog folder',lambda s:s['windows']=='2' and int(s['front']) not in (root_id, about_id, quickdraw_id))
        applications_id=int(applications['front'])
        wait_log('name=Applications')
        wait_log(f'FINDER_FOLDER_READY window={applications_id}')
        screenshot('finder-applications-folder.png')
        key('up',('ctrl',))
        passed('Command-Up activates the existing parent folder',window_count(2,root_id))
        key('n')
        passed('N selects the existing startup root without duplicates',window_count(2,root_id))
        click(104,92)
        key('o',('ctrl',))
        documents=passed('Command-O opens selected Documents catalog folder',lambda s:s['windows']=='3' and int(s['front']) not in (root_id, applications_id))
        documents_id=int(documents['front'])
        wait_log('name=Documents')
        wait_log(f'FINDER_FOLDER_READY window={documents_id}')
        screenshot('finder-documents-folder.png')
        key('w',('ctrl_r',))
        passed('right Ctrl-W produces guest Command-W',window_count(2,root_id))
        click(21,36)
        passed('close box removes the root and activates remaining folder',window_count(1,applications_id))
        key('w',('ctrl',))
        passed('closing all folder windows leaves the desktop responsive',window_count(0,0))
        move(16,444)
        empty=screenshot('finder-empty-desktop.png')
        key('n')
        fresh=passed('N reopens the mounted startup folder with a fresh ID',lambda s:s['windows']=='1' and int(s['front']) not in (root_id, about_id, quickdraw_id, applications_id, documents_id))
        fresh_id=int(fresh['front'])
        key('w',('ctrl',))
        passed('fresh root folder closes back to the desktop',window_count(0,0))
        move(16,444)
        roundtrip=screenshot('finder-empty-roundtrip.png')
        assert ImageChops.difference(empty.crop((0,21,*display_size())),roundtrip.crop((0,21,*display_size()))).getbbox() is None,'folder new/close left stale desktop pixels'
        records.append({'check':'retained desktop is restored exactly after folder open/close', 'status':'passed'})
        click(600,50);click(600,50)
        passed('double-click mounted volume desktop icon reopens its real root',lambda s:s['windows']=='1' and int(s['front']) != fresh_id)
        screenshot('finder-desktop-volume-open.png')
        key('w',('ctrl',))
        passed('desktop volume root closes cleanly',window_count(0,0))
        key('a',('shift',))
        passed('shifted keyboard event carries uppercase character',lambda s:s['key']=='65' and s['windows']=='1')
        before=int(state()['input_seq'])
        for _ in range(70):key('a')
        passed('virtqueue descriptors recycle beyond one ring without event drops',lambda s:int(s['input_seq'])>=before+140 and s['dropped']=='0')
        key('n');key('n')
        passed('repeated N selects one real root alongside About',window_count(2))
        key('q')
        passed('Q reopens QuickDraw Exerciser after filesystem and input checks',window_count(3))
        screenshot('finder-checks-complete.png')
    except BaseException as error:
        (results / 'desktop-input-checks.json').write_text(json.dumps(
            {'status': 'failed', 'checks': records, 'error': str(error), 'last_state': state()}, indent=2) + '\n')
        raise
    (results / 'desktop-input-checks.json').write_text(json.dumps(
        {'status': 'passed', 'checks': records}, indent=2) + '\n')
    return records


if __name__ == '__main__':
    check_events(json.loads((vm.STATE / 'qmp-endpoint.json').read_text()),
                 vm.STATE / 'serial.log', vm.ROOT / 'results/system7')
