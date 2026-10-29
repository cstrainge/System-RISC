"""Generated-image cleanup boundaries; all media lives in a temporary workspace."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

spec = importlib.util.spec_from_file_location(
    'image_cleanup', Path(__file__).resolve().parents[1] / 'prune_system7_images.py')
cleanup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cleanup)


class ImageCleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.original_root = cleanup.ROOT
        cleanup.ROOT = self.root

    def tearDown(self):
        cleanup.ROOT = self.original_root
        self.temp.cleanup()

    def write(self, name, data=b'fixture'):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def prune(self):
        with contextlib.redirect_stdout(io.StringIO()):
            cleanup.prune()
        return json.loads((self.root / 'results/system7/image-cleanup.json').read_text())

    def test_generated_formats_removed_without_deleting_current_media_or_evidence(self):
        old = [self.write(f'results/system7/finished/disk{suffix}')
               for suffix in ['.hfs', '.hfsb', '.hfs-b', '.qcow2', '.raw', '.img', '.hda', '.dsk']]
        old += [self.write('build/system7/dist/system7.hfs'),
                self.write('build/system7/dist/obsolete/system7.hfsb'),
                self.write('build/system7/dist/obsolete/bootrom.bin'),
                self.write('build/system7/failed/disk.hfs'),
                self.write('results/system7/finished/bootrom.bin'),
                self.write('working/system7-vm/system7-previous-123.qcow2')]
        keep = [self.write(name) for name in [
            'build/system7/dist/system7.hfsb', 'build/system7/dist/system7.qcow2',
            'build/system7/dist/bootrom.bin', 'working/system7-vm/system7.qcow2',
            'original-media/Macintosh.hfs',
            'System7/crates/mac-toolbox/tests/data/original.hfs',
            'results/system7/finished/report.json', 'results/system7/finished/frame.png',
            'results/system7/finished/serial.log']]
        link = self.root / 'results/system7/finished/reference.hfs'
        link.symlink_to(keep[4])
        report = self.prune()
        self.assertEqual(report['removed_files'], len(old))
        self.assertGreater(report['allocated_bytes_freed'], 0)
        self.assertTrue(all(not path.exists() for path in old))
        self.assertTrue(all(path.read_bytes() == b'fixture' for path in keep))
        self.assertTrue(link.is_symlink())
        self.assertEqual(self.prune()['removed_files'], 0)

    def test_user_exports_and_stopped_persistent_disks_are_not_generated_garbage(self):
        keep = [self.write('build/system7/user-state/snapshot/children/bootrom.bin'),
                self.write('build/system7/check/user-state/snapshot/source.hfsb'),
                self.write('build/system7/custom/vm/system7.qcow2')]
        self.write('build/system7/custom/vm/persistent-user-state.json', b'{"version":1}')
        obsolete = self.write('build/system7/obsolete.qcow2')
        self.prune()
        self.assertTrue(all(path.exists() for path in keep))
        self.assertFalse(obsolete.exists())

    def test_live_capture_is_preserved_while_dead_capture_is_pruned(self):
        self.write('results/system7/live/vm/pid', str(os.getpid()).encode())
        live = self.write('results/system7/live/dist/disk.hfs')
        self.write('results/system7/dead/vm/pid', b'999999999')
        dead = self.write('results/system7/dead/dist/disk.hfs')
        report = self.prune()
        self.assertTrue(live.exists())
        self.assertFalse(dead.exists())
        self.assertIn('results/system7/live', report['protected_live_captures'])

    def test_current_build_profiles_survive_other_profiles_cleanup(self):
        keep = []
        for profile in ('check', 'custom-run'):
            self.write(f'build/system7/{profile}/dist/build.json', b'{}')
            keep.extend(self.write(f'build/system7/{profile}/dist/{name}')
                        for name in ('system7.hfsb', 'system7.qcow2', 'bootrom.bin'))
        old = self.write('build/system7/check/boot-checks/unblessed.hfsb')
        self.prune()
        self.assertTrue(all(path.exists() for path in keep))
        self.assertFalse(old.exists())

    def test_generated_media_compose_with_live_current_user_and_reference_protections(self):
        self.write('build/system7/check/dist/build.json', b'{}')
        self.write('results/system7/live/pid', str(os.getpid()).encode())
        self.write('results/system7/failed/pid', b'not-a-process-id')
        keep = [self.write(name) for name in [
            'build/system7/check/dist/system7.hfsb',
            'build/system7/check/dist/system7.qcow2',
            'build/system7/check/dist/bootrom.bin',
            'results/system7/live/dist/disk.raw',
            'build/system7/user-state/source/Finder.raw',
            'working/system7-vm/system7.qcow2',
            'original-media/Macintosh.raw']]
        obsolete = self.write('results/system7/failed/dist/disk.raw')
        report = self.prune()
        self.assertTrue(all(path.exists() for path in keep))
        self.assertFalse(obsolete.exists())
        self.assertEqual(report['files'], ['results/system7/failed/dist/disk.raw'])

    def test_unlink_refusal_is_reported_without_touching_saved_or_current_media(self):
        saved = self.write('build/system7/user-state/source/Saved.raw')
        current = self.write('build/system7/dist/system7.qcow2')
        obsolete = self.write('results/system7/finished/disk.raw')
        unlink = Path.unlink

        def refuse(path, *args, **kwargs):
            if path == obsolete:
                raise PermissionError(13, 'cannot remove generated image', str(path))
            return unlink(path, *args, **kwargs)

        with patch.object(Path, 'unlink', refuse):
            with self.assertRaisesRegex(PermissionError, 'cannot remove generated image') as error:
                self.prune()
        self.assertIn(str(obsolete), str(error.exception))
        self.assertTrue(obsolete.exists())
        self.assertTrue(saved.exists())
        self.assertTrue(current.exists())
        self.assertFalse((self.root / 'results/system7/image-cleanup.json').exists(),
                         'a refused cleanup must not publish a success report')

    def test_prepared_fixture_survives_before_a_vm_pid_exists(self):
        owner = self.write('results/system7/preparing/pid', str(os.getpid()).encode())
        disk = self.write('results/system7/preparing/dist/conformance.raw')
        rom = self.write('results/system7/preparing/dist/bootrom.bin')
        report = self.prune()
        self.assertTrue(disk.exists())
        self.assertTrue(rom.exists())
        self.assertIn('results/system7/preparing', report['protected_live_captures'])
        owner.unlink()
        self.prune()
        self.assertFalse(disk.exists())
        self.assertFalse(rom.exists())

    def test_active_top_level_builder_is_protected_until_it_releases_output(self):
        owner = self.write('build/system7/pid', str(os.getpid()).encode())
        active = self.write('build/system7/image-work/system7.hfsb')
        self.prune()
        self.assertTrue(active.exists())
        owner.unlink()
        self.prune()
        self.assertFalse(active.exists())

    def test_another_owner_can_remove_a_candidate_during_cleanup(self):
        disappearing = self.write('results/system7/completing/disk.hfsb')
        unlink = Path.unlink
        def remove_elsewhere(path, *args, **kwargs):
            if path == disappearing:
                unlink(path)
                raise FileNotFoundError(path)
            return unlink(path, *args, **kwargs)
        with patch.object(Path, 'unlink', remove_elsewhere):
            report = self.prune()
        self.assertFalse(disappearing.exists())
        self.assertEqual(report['removed_files'], 0)
        self.assertEqual(report['allocated_bytes_freed'], 0)


if __name__ == '__main__':
    unittest.main()
