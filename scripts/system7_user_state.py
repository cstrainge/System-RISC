"""Preserve user files through the shared HFS-B tool, outside generated media.

A live guest is never read through an offline image tool. Rebuild may defer its
snapshot; replacing the writable desktop disk always snapshots it first.
"""
import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import shlex
import subprocess
import tempfile
import uuid

from hfs_b_tool import tool
from riscv_vm import qmp
from system7_paths import ROOT, build_dir

TREES = {'Preferences': ('/System Folder/Preferences', '/System Folder'),
         'Documents': ('/Documents', '/')}


def command(args):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f'User-file preservation failed: {shlex.join(list(map(str,args)))}\n'
                           f'{result.stderr.strip() or result.stdout.strip()}')
    return result.stdout


def hfs(operation, image, *args):
    return command([tool(), operation, image, *args])


def running(state):
    endpoint = state / 'qmp-endpoint.json'
    if endpoint.exists():
        try:
            qmp('query-status', endpoint=json.loads(endpoint.read_text()))
            return True
        except (OSError, ValueError, RuntimeError):
            pass
    try:
        os.kill(int((state / 'pid').read_text().strip()), 0)
        return True
    except (OSError, ValueError):
        return False


def default_state(build):
    override = os.environ.get('SYSTEM7_VM_STATE_DIR')
    if override:
        return Path(override).resolve()
    return ROOT / 'working/system7-vm' if build == ROOT / 'build/system7' else build / 'vm'


@contextmanager
def locked(build):
    root = build / 'user-state'
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root / 'lock').open('a') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError(f'User-file preservation is already active in {root}; retry when it finishes') from error
        yield root


def current(root):
    path = root / 'current.json'
    if not path.exists():
        return None
    try:
        value = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        raise RuntimeError(f'Cannot read user-file snapshot manifest {path}: {error}; original VM disk was not replaced') from error
    if not isinstance(value, dict) or type(value.get('version')) is not int or value['version'] != 1 \
            or not isinstance(value.get('snapshot'), str) \
            or not re.fullmatch(r'[0-9a-f]{32}', value['snapshot']) \
            or not isinstance(value.get('trees'), list) \
            or not all(isinstance(name,str) and name in TREES for name in value['trees']) \
            or len(set(value['trees'])) != len(value['trees']):
        raise RuntimeError(f'Invalid user-file snapshot manifest: {path}; original VM disk was not replaced')
    directory = root / value['snapshot']
    for name in value['trees']:
        if not (directory / name / 'metadata.json').is_file():
            raise RuntimeError(f'Incomplete user-file snapshot: {directory / name}; original VM disk was not replaced')
    return value


def children(raw, parent):
    return [json.loads(line) for line in hfs('list', raw, parent).splitlines() if line]


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def snapshot_raw(root, raw, source):
    """Publish only after both lossless recursive exports succeed."""
    name = uuid.uuid4().hex
    directory = root / name
    directory.mkdir(mode=0o700)
    published = False
    try:
        top = children(raw, '/')
        system = next((entry for entry in top if entry['name'].casefold() == 'system folder'), None)
        if system is None or not system['directory']:
            raise RuntimeError(f'Cannot preserve user files: {source} has no System Folder directory')
        system_children = children(raw, system['id'])
        present = []
        for label, (guest, _) in TREES.items():
            entries = system_children if label == 'Preferences' else top
            entry = next((entry for entry in entries if entry['name'].casefold() == label.casefold()), None)
            if entry is None:
                continue
            if not entry['directory']:
                raise RuntimeError(f'Cannot preserve {guest}: it is not a directory in {source}')
            hfs('copy-out', raw, guest, directory / label)
            present.append(label)
        # Publish only durable fork files and directory entries.
        directories = [directory]
        for path in directory.rglob('*'):
            if path.is_dir():
                directories.append(path)
            else:
                with path.open('rb') as stream:
                    os.fsync(stream.fileno())
        for path in reversed(directories):
            sync_directory(path)
        value = {'version': 1, 'snapshot': name, 'trees': present, 'source': str(source)}
        pending = root / 'current.pending'
        with pending.open('w') as stream:
            json.dump(value, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(pending, root / 'current.json')
        published = True
        sync_directory(root)
    except BaseException:
        if not published:
            shutil.rmtree(directory)
        raise
    # Keep the one current file export, not historical disk images or exports.
    for old in root.iterdir():
        if old.is_dir() and re.fullmatch(r'[0-9a-f]{32}', old.name) and old.name != name:
            shutil.rmtree(old)
    return value


def snapshot_disk(root, disk):
    with tempfile.TemporaryDirectory(prefix='snapshot-', dir=root) as folder:
        raw = Path(folder) / 'source.hfsb'
        command(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k', disk, raw])
        return snapshot_raw(root, raw, disk)


def restore_raw(root, raw):
    value = current(root)
    if value is None:
        return []
    for name in value['trees']:
        hfs('copy-in', raw, root / value['snapshot'] / name, TREES[name][1], '--replace')
    hfs('check', raw)
    return value['trees']


def prepare(build, state):
    """Called before formatting a new base image; checks use separate roots."""
    with locked(build) as root:
        disk = state / 'system7.qcow2'
        if disk.exists():
            if running(state):
                current(root)  # Invalid saved state must still fail honestly.
                print('Writable VM is running; latest Preferences/Documents will be preserved before replacement on restart.')
                return
            snapshot_disk(root, disk)
        elif current(root) is None:
            base = build / 'dist/system7.qcow2'
            if base.exists():
                snapshot_disk(root, base)


def restore(build, raw):
    with locked(build) as root:
        names = restore_raw(root, raw)
    print('Restored user folders: ' + (', '.join(names) if names else 'fresh installation'))


def mark_persistent(build, state):
    # Preserve stopped custom-profile disks too, including unchanged-base starts.
    if running(state):
        raise RuntimeError(f'Cannot use the running VM disk {state / "system7.qcow2"}; stop this VM first. User files are unchanged.')
    (state / 'persistent-user-state.json').write_text(json.dumps({'version': 1, 'build_dir': str(build)}) + '\n')


def replace_disk(build, state, base):
    """Stage, restore and check before atomically replacing the old writable disk."""
    disk = state / 'system7.qcow2'
    mark_persistent(build, state)
    with locked(build) as root:
        if disk.exists():
            snapshot_disk(root, disk)
        if current(root) is None:
            # No prior writable disk or user export exists on first launch.
            with tempfile.NamedTemporaryFile(prefix='.replacement-', dir=state, delete=False) as file:
                replacement = Path(file.name)
            try:
                shutil.copyfile(base, replacement)
                os.replace(replacement, disk)
            finally:
                replacement.unlink(missing_ok=True)
            return
        with tempfile.TemporaryDirectory(prefix='restore-', dir=root) as folder:
            raw = Path(folder) / 'restored.hfsb'
            command(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k', base, raw])
            restore_raw(root, raw)
            # Same directory/filesystem as the destination for atomic replacement.
            replacement = state / '.restored-system7.qcow2'
            try:
                command(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', raw, replacement])
                command(['qemu-img', 'check', replacement])
                os.replace(replacement, disk)
            finally:
                replacement.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'restore'])
    parser.add_argument('path', nargs='?', type=Path)
    args = parser.parse_args()
    build = build_dir()
    if args.action == 'prepare':
        prepare(build, default_state(build))
    else:
        if args.path is None:
            parser.error('restore requires a raw HFS-B image path')
        restore(build, args.path)


if __name__ == '__main__':
    main()
