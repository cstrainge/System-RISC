"""Private profiling transports; canonical Rust tests own all assertions."""
from contextlib import contextmanager
import json
import re
from pathlib import Path
import socket

import system7_vm as vm
from capture_system7_event_trace import decode


@contextmanager
def input_stream(endpoint):
    """Pace physical input without a new TCP handshake for every mouse packet."""
    if endpoint['transport'] == 'tcp':
        sock = socket.create_connection((endpoint['host'], endpoint['port']), timeout=.5)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    else:
        sock = socket.socket(socket.AF_UNIX)
        sock.settimeout(.5)
        sock.connect(endpoint['path'])
    with sock:
        sock.settimeout(5)
        with sock.makefile('rwb', buffering=0) as stream:
            greeting = json.loads(stream.readline())
            if 'QMP' not in greeting:
                raise RuntimeError('Private input profiler did not receive a QMP greeting')
            sequence = 0

            def execute(command, arguments):
                nonlocal sequence
                sequence += 1
                stream.write((json.dumps({'execute': command, 'arguments': arguments,
                                          'id': sequence}) + '\n').encode())
                while True:
                    line = stream.readline()
                    if not line:
                        raise RuntimeError('QEMU closed the private input profiling connection')
                    response = json.loads(line)
                    if response.get('id') != sequence:
                        continue
                    if 'error' in response:
                        raise RuntimeError(f"Private input command {command}: {response['error']}")
                    return response['return']

            execute('qmp_capabilities', {})
            yield lambda events: execute('input-send-event', {'events': events})


def snapshot(endpoint, serial, output, name):
    marker = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2',
                       Path(serial).read_text(errors='replace'))
    if not marker:
        raise RuntimeError('This capture requires a System image built with SYSTEM7_EVENT_TRACE=1')
    path = Path(output) / (name + '.bin')
    filename = vm.windows_path(path) if endpoint.get('platform') == 'windows' else str(path)
    vm.qmp('stop', endpoint=endpoint)
    try:
        vm.qmp('pmemsave', {'val': int(marker[1]), 'size': int(marker[2]), 'filename': filename}, endpoint)
    finally:
        vm.qmp('cont', endpoint=endpoint)
    return decode(path)
