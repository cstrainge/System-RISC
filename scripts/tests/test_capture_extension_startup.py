"""Native startup capture must preserve causes and unreaped private media."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import capture_extension_startup as capture
import system7_capture


class NativeStartupCaptureTests(unittest.TestCase):
    def test_discovery_uses_each_elf_symbol_and_rejects_missing_or_ambiguous_entries(self):
        for symbols in ['', '1 t ' + capture.DISCOVERY + '\n2 t ' + capture.DISCOVERY + '\n']:
            with patch.object(capture, 'run', return_value=Mock(stdout=symbols)):
                with self.assertRaisesRegex(RuntimeError, 'exactly one.*rebuild'):
                    capture.discovery_entry(Path('/private/System.elf'))
        with patch.object(capture, 'run', return_value=Mock(
                stdout='123abc t ' + capture.DISCOVERY + '\n')) as run:
            self.assertEqual(capture.discovery_entry(Path('/private/System.elf')), 0x123abc)
        self.assertEqual(run.call_args.args[0], ['nm', '-C', '/private/System.elf'])

    def gdb_launch_failure(self, root, reaped):
        installation = root / 'installation'
        base = installation / 'dist'
        base.mkdir(parents=True)
        for name in ['system7.qcow2', 'bootrom.bin', 'System.elf', 'System.rsrc']:
            (base / name).write_bytes(b'untouched original')
        output = root / 'capture'
        output.mkdir()
        child = Mock(pid=12345, stdout=None)
        child.poll.return_value = None

        def run(arguments, **options):
            Path(arguments[-1]).write_bytes(b'private generated disk')

        def stop(process, endpoint):
            self.assertIs(process, child)
            self.assertEqual(endpoint['transport'], 'unix')
            self.assertTrue(endpoint['path'].startswith('/tmp/srisc-init-'))
            if not reaped:
                raise RuntimeError('recorded-child-reap-failed')
            child.poll.return_value = 0

        with patch.dict(os.environ, {'SYSTEM7_BUILD_DIR': str(installation)}), \
             patch.object(capture, 'prepare'), \
             patch.object(capture, 'run', side_effect=run), \
             patch.object(capture, 'Image') as image, \
             patch.object(capture, 'discovery_entry', return_value=0x123abc), \
             patch.object(capture, 'wait_until'), \
             patch.object(capture.subprocess, 'Popen', side_effect=[
                 child, RuntimeError('private-GDB-spawn-refused')]), \
             patch.object(system7_capture, 'stop_owned', side_effect=stop) as stopped:
            image.return_value.run.return_value = ''
            with self.assertRaises(BaseExceptionGroup) as raised:
                capture.capture(output)
        stopped.assert_called_once()
        self.assertIn('private-GDB-spawn-refused', str(raised.exception.exceptions[0]))
        report = json.loads((output / 'capture.json').read_text())
        self.assertEqual(report['original_before'], report['original_after'])
        self.assertIn('private-GDB-spawn-refused', report['capture_error'])
        for path in base.iterdir():
            self.assertEqual(path.read_bytes(), b'untouched original')
        return output, report, raised.exception

    def test_gdb_launch_failure_reaps_the_already_owned_vm_and_prunes_private_disks(self):
        with tempfile.TemporaryDirectory() as folder:
            output, report, _ = self.gdb_launch_failure(Path(folder), reaped=True)
            self.assertEqual(report['owners'], [{'pid': 12345, 'reaped': True, 'exit_code': 0}])
            self.assertEqual(report['cleanup_errors'], [])
            self.assertNotIn('media_quarantine', report)
            self.assertFalse(list((output / 'dist').glob('*.raw')))
            self.assertFalse(list((output / 'vm').glob('*.qcow2')))
            self.assertFalse((output / 'vm/bootrom.bin').exists())

    def test_gdb_launch_and_reap_failure_preserve_both_causes_and_exact_media(self):
        with tempfile.TemporaryDirectory() as folder:
            output, report, _ = self.gdb_launch_failure(Path(folder), reaped=False)
            self.assertEqual(report['owners'], [{'pid': 12345, 'reaped': False, 'exit_code': None}])
            self.assertTrue(any('recorded-child-reap-failed' in error
                                for error in report['cleanup_errors']))
            self.assertEqual(set(report['media_quarantine']), {
                str(output / 'dist/startup.raw'), str(output / 'vm/system7.qcow2'),
                str(output / 'vm/bootrom.bin')})
            self.assertTrue(all(Path(path).is_file() for path in report['media_quarantine']))
            # The injected child is a Mock; retire only this test's empty sockets.
            endpoint = json.loads((output / 'vm/qmp-endpoint.json').read_text())
            Path(endpoint['path']).parent.rmdir()

    def test_socket_removal_failure_keeps_original_capture_cause_and_writes_report(self):
        with tempfile.TemporaryDirectory() as folder:
            with patch.object(capture.shutil, 'rmtree',
                              side_effect=OSError('private-socket-removal-refused')):
                output, report, failures = self.gdb_launch_failure(Path(folder), reaped=True)
            self.assertIn('private-GDB-spawn-refused', str(failures.exceptions[0]))
            self.assertTrue(any('private-socket-removal-refused' in str(error)
                                for error in failures.exceptions))
            self.assertTrue((output / 'capture.json').is_file())
            self.assertTrue(any('private-socket-removal-refused' in error
                                for error in report['cleanup_errors']))
            self.assertEqual(report['owners'], [{'pid': 12345, 'reaped': True, 'exit_code': 0}])
            self.assertFalse((output / 'vm/system7.qcow2').exists())
            endpoint = json.loads((output / 'vm/qmp-endpoint.json').read_text())
            Path(endpoint['path']).parent.rmdir()


if __name__ == '__main__':
    unittest.main()
