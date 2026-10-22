"""Bounded failure evidence from an owned private VM, never acceptance decisions."""
import json
import re
import socket
import struct
import subprocess
from functools import cache

import system7_vm as vm


@cache
def heap_address(dist):
    symbols = subprocess.run(['llvm-nm', '-C', str(dist / 'System.elf')],
                             capture_output=True, text=True, check=True)
    return int(next(line.split()[0] for line in symbols.stdout.splitlines()
                    if line.endswith('mac_system::kernel::HEAP')), 16)


def heap_usage(endpoint, dist):
    try:
        address = heap_address(dist)
        words = vm.qmp('human-monitor-command', {'command-line': f'xp /7gx 0x{address:x}'}, endpoint)
        values = [int(v, 16) for v in re.findall(r'0x([0-9a-fA-F]{16})', words)]
        if len(values) == 7:
            return {'used': values[1], 'capacity': values[5] - values[4]}
    except (OSError, StopIteration, subprocess.CalledProcessError):
        pass
    return None


def capture_failure(output, driver, dist):
    from capture_system7_event_trace import decode
    vm.qmp('stop', endpoint=driver.endpoint)
    registers = vm.qmp('human-monitor-command', {'command-line': 'info registers'},
                       driver.endpoint)
    (output / 'failure-registers.txt').write_text(registers)
    try:
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        vm.qmp('human-monitor-command', {'command-line': f'gdbserver tcp:127.0.0.1:{port}'},
               driver.endpoint)
        debug = subprocess.run(['gdb-multiarch', '--batch', '--nx', '--quiet',
                                str(dist / 'System.elf'),
                                '-ex', f'target remote 127.0.0.1:{port}',
                                '-ex', 'set print elements 4',
                                '-ex', 'set print max-depth 5',
                                '-ex', 'bt 24',
                                '-ex', 'set language c',
                                '-ex', "p 'mac_system::kernel::DESKTOP'",
                                '-ex', 'detach'], capture_output=True, text=True, timeout=10)
        (output / 'failure-gdb.txt').write_text(debug.stdout + debug.stderr)
        vm.qmp('stop', endpoint=driver.endpoint)
    except (OSError, subprocess.TimeoutExpired):
        pass
    marker = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2', driver.log())
    if marker:
        path = output / 'failure.trace.bin'
        vm.qmp('pmemsave', {'val': int(marker[1]), 'size': int(marker[2]),
                           'filename': str(path)}, driver.endpoint)
        (output / 'failure.trace.json').write_text(json.dumps(decode(path), indent=2) + '\n')
    # Optional host LLVM support enriches a failed capture; it is not required
    # for launching the OS or deciding whether a conformance assertion passed.
    try:
        symbols = subprocess.run(['llvm-nm', '-C', str(dist / 'System.elf')],
                                 capture_output=True, text=True, check=True)
        heap = next(line.split()[0] for line in symbols.stdout.splitlines()
                    if line.endswith('mac_system::kernel::HEAP'))
        words = vm.qmp('human-monitor-command', {'command-line': f'xp /7gx 0x{heap}'},
                       driver.endpoint)
        (output / 'failure-heap.txt').write_text(words)
        values = [int(value, 16) for value in re.findall(r'0x([0-9a-fA-F]{16})', words)]
        if len(values) != 7 or not 0 < values[5] - values[4] <= 16 * 1024 * 1024:
            return
        bottom, top = values[4:6]
        raw = output / 'failure-heap.bin'
        vm.qmp('pmemsave', {'val': bottom, 'size': top - bottom, 'filename': str(raw)},
               driver.endpoint)
        memory = raw.read_bytes()
        holes, seen, address = [], set(), values[3]
        while bottom <= address <= top - 16 and address not in seen:
            seen.add(address)
            at = address
            size, address = struct.unpack_from('<QQ', memory, at - bottom)
            holes.append({'address': at, 'size': size, 'next': address})
        (output / 'failure-heap.json').write_text(json.dumps({
            'used': values[1], 'free': sum(h['size'] for h in holes),
            'largest_free': max((h['size'] for h in holes), default=0),
            'holes': holes, 'invalid_or_cyclic_tail': address != 0}, indent=2) + '\n')
    except (OSError, StopIteration, subprocess.CalledProcessError):
        pass
