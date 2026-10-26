"""Explicit host exports and the lifetime of one VM's companion process."""
import argparse
from pathlib import Path
import queue
import subprocess
import threading

from system7_paths import SOURCE, host_dir, host_tool
from system7_host import process_options


def readiness(process, log_path, timeout=10):
    result = queue.Queue(maxsize=1)
    def read():
        try:
            result.put(process.stdout.readline(256))
        except Exception as error:
            result.put(error)
    reader = threading.Thread(target=read, name='host-companion-readiness', daemon=True)
    reader.start()
    try:
        line = result.get(timeout=timeout)
    except queue.Empty as error:
        raise TimeoutError('host companion did not become ready') from error
    if isinstance(line, Exception):
        raise RuntimeError(f'cannot read host companion readiness: {line}') from line
    ready = line.decode('utf-8', errors='replace').strip()
    prefix = 'HOST_BRIDGE_READY 127.0.0.1:'
    if not ready.startswith(prefix):
        raise RuntimeError('host companion failed: ' + log_path.read_text(errors='replace'))
    try:
        port = int(ready[len(prefix):])
    except ValueError as error:
        raise RuntimeError(f'invalid companion loopback port: {ready}') from error
    if not 0 < port < 65536:
        raise RuntimeError('invalid companion loopback port')
    reader.join()
    return port


def export_argument(value):
    name, separator, folder = value.partition('=')
    if not separator or not name or not folder:
        raise argparse.ArgumentTypeError('export must be NAME=PATH')
    path = Path(folder).expanduser().absolute()
    if not path.is_dir():
        raise argparse.ArgumentTypeError(f'export directory does not exist: {path}')
    return f'{name}={path}'


def stop(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
    if process.stdout is not None:
        process.stdout.close()


def start(state, writable, readonly, *, initialize=None):
    """Caller stops this process on launch failure; VM disconnect ends success."""
    count = len(writable) + len(readonly)
    if not 1 <= count <= 16:
        raise ValueError('select between one and sixteen explicit host exports')
    command = ['cargo', 'build', '--locked', '--release', '-p', 'risc-host']
    if initialize is not None:
        command.extend(['-p', 'risc-forks'])
    subprocess.run([*command, '--target-dir', str(host_dir())], cwd=SOURCE, check=True)
    if initialize is not None and not (initialize / '.system-risc-root').exists():
        # The Rust formatter refuses nonempty directories. Never erase/import
        # ordinary host files implicitly or replace an existing volume record.
        subprocess.run([str(host_tool('risc-forks', 'release')), 'init', str(initialize),
                        'Transfer'], check=True)
    state.mkdir(parents=True, exist_ok=True)
    arguments = [str(host_tool('risc-host', 'release')), '--one-session']
    for flag, values in [('--export', writable), ('--read-only-export', readonly)]:
        for value in values:
            arguments.extend([flag, value])
    log_path = state / 'host-companion.log'
    with log_path.open('wb') as log:
        process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=log, **process_options())
    try:
        port = readiness(process, log_path)
        # There is exactly one readiness line; ordinary diagnostics go to stderr.
        process.stdout.close()
        return process, port
    except BaseException:
        stop(process)
        raise
