"""Private child ownership and generated-media quarantine on teardown failure."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import system7_capture as capture


class PrivateCaptureTests(unittest.TestCase):
    def child(self, code=None, pid=123):
        child = Mock(pid=pid, stdout=None)
        child.poll.return_value = code
        return child

    def media(self, output):
        folder = output / 'vm'
        folder.mkdir()
        disks = [folder / ('private' + suffix) for suffix in capture.DISK_SUFFIXES]
        disks.append(folder / 'bootrom.bin')
        for disk in disks:
            disk.write_bytes(b'private test media')
        (folder / 'evidence.bin').write_bytes(b'retained evidence')
        (output / 'original.qcow2').write_bytes(b'reference outside owned media roots')
        return folder, disks

    def test_failed_launch_retains_exact_spawned_child_without_a_default_endpoint(self):
        child = self.child(0)
        owned = capture.OwnedProcesses()
        def launch(*args, **options):
            capture.vm.subprocess.Popen(['private-qemu'])
            raise RuntimeError('private launch failed after spawn')
        with patch.object(capture.subprocess, 'Popen', return_value=child), \
             patch.object(capture.vm, 'launch', side_effect=launch):
            with self.assertRaisesRegex(RuntimeError, 'failed after spawn'):
                owned.launch('private-state')
        self.assertIs(owned.records[0]['process'], child)
        self.assertIsNone(owned.records[0]['endpoint'])
        with tempfile.TemporaryDirectory() as folder, patch.object(capture.vm, 'qmp') as qmp:
            owned.cleanup(Path(folder), [])
        qmp.assert_not_called()

    def test_any_unreaped_owner_quarantines_every_private_disk_and_attempts_other_cleanup(self):
        live, stopped = self.child(None, 123), self.child(0, 456)
        owned = capture.OwnedProcesses()
        owned.add(live)
        owned.add(stopped)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            media, disks = self.media(output)
            with patch.object(capture, 'stop_owned', side_effect=[OSError('owned child refuses stop'), None]) as stop:
                with self.assertRaisesRegex(ExceptionGroup, 'cleanup failed') as caught:
                    owned.cleanup(output, [media])
            self.assertEqual(stop.call_count, 2)
            self.assertTrue(all(disk.is_file() for disk in disks))
            report = json.loads((output / 'cleanup.json').read_text())
            self.assertTrue(report['quarantined'])
            self.assertEqual([owner['reaped'] for owner in report['owners']], [False, True])
            self.assertIn('owned child refuses stop', report['errors'])
            self.assertIn('positively reaped', report['errors'][1])
            self.assertEqual(len(caught.exception.exceptions), 2)

    def test_positive_reap_prunes_all_disk_formats_but_preserves_reports_and_reference(self):
        owned = capture.OwnedProcesses()
        owned.add(self.child(0))
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            media, disks = self.media(output)
            report = owned.cleanup(output, [media])
            self.assertFalse(report['quarantined'])
            self.assertEqual(report['errors'], [])
            self.assertTrue(all(not disk.exists() for disk in disks))
            self.assertEqual((media / 'evidence.bin').read_bytes(), b'retained evidence')
            self.assertTrue((output / 'original.qcow2').is_file())

    def test_capture_cause_and_all_cleanup_causes_survive_a_confirmed_reap(self):
        child = self.child(0)
        child.stdout = Mock()
        child.stdout.close.side_effect = OSError('owned stdout close failed')
        owned = capture.OwnedProcesses()
        owned.add(child)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            media, disks = self.media(output)
            with patch.object(capture, 'stop_owned', side_effect=OSError('stop acknowledgement failed')):
                with self.assertRaises(ExceptionGroup) as caught:
                    try:
                        raise RuntimeError('original capture cause')
                    finally:
                        owned.cleanup(output, [media])
            self.assertEqual([str(error) for error in caught.exception.exceptions],
                             ['original capture cause', 'stop acknowledgement failed',
                              'owned stdout close failed'])
            self.assertTrue(all(not disk.exists() for disk in disks))

    def test_unknown_endpoint_never_queries_the_regular_monitor_and_kill_is_bounded(self):
        child = self.child()
        child.wait.side_effect = [capture.subprocess.TimeoutExpired('owned', 10), 0]
        with patch.object(capture.vm, 'qmp') as qmp:
            capture.stop_owned(child)
        qmp.assert_not_called()
        child.terminate.assert_called_once_with()
        child.kill.assert_called_once_with()
        self.assertEqual([call.kwargs for call in child.wait.call_args_list],
                         [{'timeout': 10}, {'timeout': 10}])

    def test_explicit_quit_waits_for_graceful_exit_even_when_qmp_drops_its_reply(self):
        for error in (None, ConnectionResetError('quit closed its owned monitor')):
            with self.subTest(error=error):
                child = self.child()
                endpoint = {'transport': 'tcp', 'host': '127.0.0.1', 'port': 12345}
                with patch.object(capture.vm, 'qmp', side_effect=error) as qmp:
                    capture.stop_owned(child, endpoint)
                qmp.assert_called_once_with('quit', endpoint=endpoint)
                child.wait.assert_called_once_with(timeout=2)
                child.terminate.assert_not_called()
                child.kill.assert_not_called()


if __name__ == '__main__':
    unittest.main()
