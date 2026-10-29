"""Read consistent UI acknowledgements without UART traffic or a debug trap.

Read the System-owned atomic sequence snapshot through its actual kernel page
tables and physical memory. A selected CPU may currently use firmware roots;
its virtual debugger view is therefore not a stable kernel-memory reader.
The guest keeps running; a second sequence read detects overlapping publication.
This is capture transport only, not a replacement guest service or OS acceptance.
"""
from functools import lru_cache
import json
import re
import time
from pathlib import Path
import system7_vm as vm


def physical_words(endpoint, address, count):
    text = vm.qmp('human-monitor-command',
                  {'command-line': f'xp /{count}gx {address:#x}'}, endpoint)
    values = [int(value, 16) for value in re.findall(r'0x([0-9a-fA-F]{16})', text)]
    if len(values) != count:
        raise RuntimeError(f'Cannot read {count} physical observer words at {address:#x}: ' + text)
    return values


@lru_cache(maxsize=32)
def observer_page(endpoint_key, serial, epoch, root, virtual_page):
    # Only the permanently mapped System observer is cached, per boot marker
    # and owned capture endpoint. No application mapping is assumed immutable.
    if root == 0 or root % 4096:
        raise RuntimeError(f'Invalid published kernel page-table root {root:#x}')
    sign = (virtual_page >> 38) & 1
    if virtual_page >> 39 != ((1 << 25) - 1 if sign else 0):
        raise RuntimeError(f'Observer address {virtual_page:#x} is not canonical Sv39')
    table = root
    for level in (2, 1, 0):
        index = (virtual_page >> (12 + level * 9)) & 511
        entry = physical_words(json.loads(endpoint_key), table + index * 8, 1)[0]
        if not entry & 1 or entry & 4 and not entry & 2 or entry >> 54:
            raise RuntimeError(f'Invalid observer PTE at level {level}: {entry:#x}')
        physical = ((entry >> 10) & ((1 << 44) - 1)) << 12
        if entry & (2 | 8):
            mask = (1 << (12 + level * 9)) - 1
            if physical & mask:
                raise RuntimeError(f'Misaligned observer superpage at level {level}: {entry:#x}')
            if not entry & 2:
                raise RuntimeError('System UI observer mapping is not readable')
            return physical | (virtual_page & mask)
        table = physical
    raise RuntimeError('System UI observer page table has no readable leaf')


def read(endpoint, serial):
    serial = str(Path(serial).resolve())
    log = Path(serial).read_text(errors='replace')
    markers = list(re.finditer(r'UI_OBSERVER address=(\d+) bytes=128 version=1', log))
    if not markers:
        raise RuntimeError('System did not publish its UI observer address')
    marker = markers[-1]
    roots = re.findall(r'^MMU_READY mode=sv39 .*?kernel_root=(0x[0-9a-fA-F]+)\b',
                       log[:marker.start()], re.M)
    if not roots:
        raise RuntimeError('System did not publish an Sv39 kernel root before its UI observer')
    root = int(roots[-1], 16)
    address = int(marker.group(1))
    endpoint_key = json.dumps(endpoint, sort_keys=True)
    if address % 8:
        raise RuntimeError('Published UI observer is not aligned for atomic words')

    def words(count):
        values = []
        while len(values) < count:
            virtual = address + len(values) * 8
            page = virtual & ~4095
            physical = observer_page(endpoint_key, serial, marker.start(), root, page)
            offset = virtual & 4095
            chunk = min(count - len(values), (4096 - offset) // 8)
            values.extend(physical_words(endpoint, physical + offset, chunk))
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
