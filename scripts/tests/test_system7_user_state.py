"""Real HFS-B lossless user-file rebuild/replacement workflows and failure paths."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import hfs_b_tool
import system7_user_state as user


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class PreservationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = hfs_b_tool.tool()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='system risc user files ')
        self.root = Path(self.temp.name)
        self.build = self.root / 'build'
        self.state = self.build / 'vm'
        self.state.mkdir(parents=True)
        self.patch = patch.object(user, 'tool', return_value=self.binary)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.addCleanup(self.temp.cleanup)

    def volume(self, name, extra=False):
        raw = self.root / name
        user.hfs('create', raw, 8 * 1024 * 1024, 'Preservation Test')
        if extra:
            user.hfs('mkdir', raw, '/', 'Changes catalog IDs')
        user.hfs('mkdir', raw, '/', 'System Folder')
        user.hfs('mkdir', raw, '/System Folder', 'Preferences')
        user.hfs('mkdir', raw, '/', 'Documents')
        return raw

    def put(self, raw, parent, name, content, resources=b''):
        with tempfile.TemporaryDirectory(dir=self.root) as folder:
            bundle = Path(folder) / 'bundle'
            bundle.mkdir()
            (bundle / 'data').write_bytes(content)
            (bundle / 'resources').write_bytes(resources)
            finder = list(b'JSONTEDT' + bytes(range(8, 32)))
            (bundle / 'metadata.json').write_text(json.dumps({
                'format': 'system-risc-forks', 'version': 1, 'directory': False,
                'name': name, 'created_mac_microseconds': 3875500000123456,
                'modified_mac_microseconds': 3875500010654321, 'backup_mac_microseconds': None,
                'finder_info': finder, 'flags': 0, 'comment': 'Keep this comment',
                'data_length': len(content), 'resource_length': len(resources)}))
            user.hfs('copy-in', raw, bundle, parent, '--replace')

    def pack(self, raw, target):
        user.command(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', raw, target])

    def unpack(self, disk):
        raw = self.root / ('read-' + os.urandom(4).hex() + '.hfsb')
        user.command(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', disk, raw])
        return raw

    def read(self, raw, path):
        with tempfile.TemporaryDirectory(dir=self.root) as folder:
            bundle = Path(folder) / 'file'
            user.hfs('copy-out', raw, path, bundle)
            metadata = json.loads((bundle / 'metadata.json').read_text())
            # Catalog identities regenerate; all user metadata remains exact.
            for key in ['id', 'parent']:
                metadata.pop(key)
            return ((bundle / 'data').read_bytes(), (bundle / 'resources').read_bytes(), metadata)

    def test_rebuild_and_replacement_keep_latest_extensionless_preferences_and_documents(self):
        old = self.volume('old.hfsb')
        self.put(old, '/System Folder/Preferences', 'Text Edit Preferences', b'{"font_size":18}', b'pref-resource')
        user.hfs('mkdir', old, '/Documents', 'CAFÉ')
        self.put(old, '/Documents/CAFÉ', 'Notes', b'Both forks survive', b'document-resource')
        self.put(old, '/System Folder', 'System', b'old system must not be restored')
        disk = self.state / 'system7.qcow2'
        self.pack(old, disk)
        original = sha(disk)
        user.prepare(self.build, self.state)
        self.assertEqual(sha(disk), original)
        fresh = self.volume('fresh.hfsb', extra=True)
        self.put(fresh, '/System Folder/Preferences', 'File Types', b'new default')
        self.put(fresh, '/System Folder', 'System', b'new system')
        user.restore(self.build, fresh)
        for path in ['/System Folder/Preferences/Text Edit Preferences', '/Documents/CAFÉ/Notes']:
            self.assertEqual(self.read(old, path), self.read(fresh, path))
        self.assertEqual(self.read(fresh, '/System Folder/Preferences/File Types')[0], b'new default')
        self.assertEqual(self.read(fresh, '/System Folder/System')[0], b'new system')
        # The guest saved again after the base was built. That newest value wins.
        self.put(old, '/System Folder/Preferences', 'Text Edit Preferences', b'{"font_size":24}', b'latest-resource')
        self.pack(old, disk)
        base = self.root / 'new-base.qcow2'
        self.pack(fresh, base)
        base_hash = sha(base)
        user.replace_disk(self.build, self.state, base)
        restored = self.unpack(disk)
        self.assertEqual(self.read(restored, '/System Folder/Preferences/Text Edit Preferences'),
                         self.read(old, '/System Folder/Preferences/Text Edit Preferences'))
        self.assertEqual(self.read(restored, '/Documents/CAFÉ/Notes'), self.read(old, '/Documents/CAFÉ/Notes'))
        self.assertEqual(sha(base), base_hash)
        self.assertTrue((self.state / 'persistent-user-state.json').exists())
        snapshots = [p for p in (self.build / 'user-state').iterdir() if p.is_dir()]
        self.assertEqual(len(snapshots), 1)

    def test_live_guest_is_deferred_and_cannot_be_overwritten(self):
        disk = self.state / 'system7.qcow2'
        disk.write_bytes(b'live guest')
        with patch.object(user, 'running', return_value=True), patch.object(user, 'snapshot_disk') as snapshot:
            user.prepare(self.build, self.state)
            snapshot.assert_not_called()
            with self.assertRaisesRegex(RuntimeError, 'running VM disk.*stop this VM first'):
                user.replace_disk(self.build, self.state, self.root / 'base')
        self.assertEqual(disk.read_bytes(), b'live guest')

    def test_failed_export_retains_previous_complete_snapshot_and_original_disk(self):
        raw = self.volume('source.hfsb')
        self.put(raw, '/Documents', 'Note', b'keep me')
        disk = self.state / 'system7.qcow2'
        self.pack(raw, disk)
        user.prepare(self.build, self.state)
        pointer = self.build / 'user-state/current.json'
        saved = pointer.read_bytes()
        disk_hash = sha(disk)
        real = user.hfs
        def fail(operation, image, *args):
            if operation == 'copy-out' and Path(args[-1]).name == 'Documents':
                raise RuntimeError('Cannot export Documents: simulated storage read failure')
            return real(operation, image, *args)
        with patch.object(user, 'hfs', side_effect=fail):
            with self.assertRaisesRegex(RuntimeError, 'Documents.*storage read failure'):
                user.prepare(self.build, self.state)
        self.assertEqual(pointer.read_bytes(), saved)
        self.assertEqual(sha(disk), disk_hash)
        self.assertEqual(len([p for p in pointer.parent.iterdir() if p.is_dir()]), 1)

    def test_failed_restore_leaves_old_disk_and_snapshot_available_for_retry(self):
        raw = self.volume('source.hfsb')
        self.put(raw, '/Documents', 'Note', b'keep me')
        disk = self.state / 'system7.qcow2'
        self.pack(raw, disk)
        user.prepare(self.build, self.state)
        base = self.root / 'base.qcow2'
        self.pack(self.volume('fresh.hfsb'), base)
        old_hash = sha(disk)
        real = user.hfs
        def fail(operation, image, *args):
            if operation == 'copy-in':
                raise RuntimeError('Cannot restore Preferences: simulated write failure')
            return real(operation, image, *args)
        with patch.object(user, 'hfs', side_effect=fail):
            with self.assertRaisesRegex(RuntimeError, 'Preferences.*write failure'):
                user.replace_disk(self.build, self.state, base)
        self.assertEqual(sha(disk), old_hash)
        self.assertTrue((self.build / 'user-state/current.json').exists())
        user.replace_disk(self.build, self.state, base)
        self.assertEqual(self.read(self.unpack(disk), '/Documents/Note')[0], b'keep me')

    def test_invalid_snapshot_is_an_error_and_profiles_are_independent(self):
        root = self.build / 'user-state'
        root.mkdir()
        (root / 'current.json').write_text('{"version":1,"snapshot":"../escape","trees":[]}')
        with self.assertRaisesRegex(RuntimeError, 'Invalid user-file snapshot manifest'):
            user.restore(self.build, self.volume('fresh.hfsb'))
        other = self.root / 'other-profile'
        user.prepare(other, other / 'vm')
        self.assertIsNone(user.current(other / 'user-state'))
        self.assertNotEqual(user.default_state(self.build), user.default_state(other))


if __name__ == '__main__':
    unittest.main()
