"""Generated-image cleanup boundaries; all media lives in a temporary workspace."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest

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
               for suffix in ['.hfs', '.qcow2', '.raw', '.img', '.hda', '.dsk']]
        old += [self.write('build/system7/failed/disk.hfs'),
                self.write('results/system7/finished/bootrom.bin'),
                self.write('working/system7-vm/system7-previous-123.qcow2')]
        keep = [self.write(name) for name in [
            'build/system7/dist/system7.hfs', 'build/system7/dist/system7.qcow2',
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

    def test_live_capture_is_preserved_while_dead_capture_is_pruned(self):
        self.write('results/system7/live/vm/pid', str(os.getpid()).encode())
        live = self.write('results/system7/live/dist/disk.hfs')
        self.write('results/system7/dead/vm/pid', b'999999999')
        dead = self.write('results/system7/dead/dist/disk.hfs')
        report = self.prune()
        self.assertTrue(live.exists())
        self.assertFalse(dead.exists())
        self.assertIn('results/system7/live', report['protected_live_captures'])


if __name__ == '__main__':
    unittest.main()
