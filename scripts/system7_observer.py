"""Read consistent UI acknowledgements without UART traffic or a debug trap.

QMP's virtual-memory inspector on hart0 reads the System-owned atomic sequence snapshot.
The guest keeps running; a second sequence read detects overlapping publication.
This is capture transport only. Canonical Rust SDK tests decide acceptance.
"""
import re
import time
from pathlib import Path
import system7_vm as vm


def read(endpoint, serial):
    marker = re.search(r'UI_OBSERVER address=(\d+) bytes=128 version=1',
                       Path(serial).read_text(errors='replace'))
    if not marker:
        raise RuntimeError('System did not publish its UI observer address')
    address = int(marker.group(1))

    def words(count):
        text = vm.qmp('human-monitor-command',
                      {'cpu-index': 0, 'command-line': f'x /{count}gx {address:#x}'}, endpoint)
        values = [int(value, 16) for value in re.findall(r'0x([0-9a-fA-F]{16})', text)]
        if len(values) != count:
            raise RuntimeError('Cannot read System UI observer: ' + text)
        return values

    for _ in range(100):
        data = words(16)
        if data[0] & 1 or words(1)[0] != data[0]:
            # A preempted writer may keep the sequence odd for a time slice.
            # Yield host transport time instead of hammering HMP while it resumes.
            time.sleep(.001)
            continue
        return decode(data)
    # A busy compositor can publish between every pair of HMP round trips.
    # Freeze only this private capture VM for one coherent read; never accept
    # a torn snapshot or resume a VM that was already paused by its caller.
    running = vm.qmp('query-status', endpoint=endpoint)['running']
    for _ in range(100):
        if running:
            vm.qmp('stop', endpoint=endpoint)
        try:
            data = words(16)
            if not data[0] & 1:
                return decode(data)
        finally:
            if running:
                vm.qmp('cont', endpoint=endpoint)
        if not running:
            break
        time.sleep(.001)
    raise RuntimeError('System UI observer did not yield a consistent snapshot')


def decode(data):
    signed = lambda value: (value & 0xffff) - (0x10000 if value & 0x8000 else 0)
    pair = lambda value: f'{signed(value)},{signed(value >> 16)}'
    action = (data[11].to_bytes(8, 'little') + data[12].to_bytes(8, 'little'))
    return {'input_seq': str(data[1]), 'scene_rev': str(data[2]),
            'mouse': pair(data[3]), 'windows': str(data[4]), 'front': str(data[5]),
            'key': str(data[6]), 'first': pair(data[7]) + ',' + pair(data[8]),
            'updates': str(data[9]), 'dropped': str(data[10]),
            'action': action.rstrip(b'\0').decode('ascii'), 'menu_paints': str(data[13]),
            'presentations': str(data[14]), 'compositions': str(data[15])}
