"""Never prune media whose privately spawned VM has not been reaped."""
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
if sys.platform == 'win32':
    capture = None
else:
    import capture_vdagent_transport as capture


@unittest.skipIf(capture is None, 'the private native capture tool requires POSIX fcntl locking')
class CaptureMediaCleanupTests(unittest.TestCase):
    def test_real_malformed_peer_closes_without_a_client_and_reaps_its_thread(self):
        peer = capture.MalformedPeer()
        began = time.monotonic()
        peer.close()
        self.assertFalse(peer.thread.is_alive())
        self.assertLess(time.monotonic() - began, 2)
        self.assertEqual(peer.errors, [])
        self.assertEqual(peer.listener.fileno(), -1)

    def test_malformed_peer_constructor_closes_listener_when_thread_start_fails(self):
        listener = capture.socket.socket()
        thread = Mock()
        thread.start.side_effect = RuntimeError('private malformed-peer thread start refused')
        with patch.object(capture.socket, 'socket', return_value=listener), \
             patch.object(capture.threading, 'Thread', return_value=thread):
            with self.assertRaisesRegex(RuntimeError, 'thread start refused'):
                capture.MalformedPeer()
        self.assertEqual(listener.fileno(), -1)

    def capture_with_stop_failure(self, root, child, launch_failure=False):
        base = root / 'base'
        base.mkdir()
        (base / 'system7.qcow2').write_bytes(b'original disk')
        (base / 'bootrom.bin').write_bytes(b'original ROM')
        output = root / 'owned-capture'
        def run(arguments):
            Path(arguments[-1]).write_bytes(b'private generated disk')
        def launch(state, **options):
            state.mkdir()
            serial = state / 'serial.log'
            serial.write_text('private fixture evidence')
            process = capture.vm.subprocess.Popen(
                ['private-qemu', '-uuid', 'owned-test', '-display', 'none', '-serial', 'file:' + str(serial)],
                env={})
            (state / 'pid').write_text(str(process.pid))
            if launch_failure:
                raise RuntimeError('private launch failed after spawning its child')
            return process, {'transport': 'tcp', 'host': '127.0.0.1', 'port': 12345}
        with patch.object(capture, 'run', side_effect=run), \
             patch.object(capture, 'Image'), \
             patch.object(capture.subprocess, 'Popen', return_value=child), \
             patch.object(capture.vm, 'launch', side_effect=launch), \
             patch.object(capture, 'stop_owned', side_effect=OSError('private child stop failed')) as stop:
            with self.assertRaises(ExceptionGroup):
                capture.capture_case(output, 'absent', base)
        self.assertEqual((base / 'system7.qcow2').read_bytes(), b'original disk')
        self.assertEqual((base / 'bootrom.bin').read_bytes(), b'original ROM')
        stop.assert_called_once()
        self.assertIs(stop.call_args.args[0], child)
        return output, json.loads((output / 'case.json').read_text()), stop.call_args.args[1]

    def test_launch_exception_retains_spawned_unreaped_child_and_exact_media(self):
        child = Mock(pid=12345)
        child.poll.return_value = None
        with tempfile.TemporaryDirectory() as folder:
            output, report, endpoint = self.capture_with_stop_failure(Path(folder), child, launch_failure=True)
            self.assertIsNone(endpoint)
            self.assertIn('launch failed after spawning', report['capture_error'])
            self.assertEqual(report['media_owners'], [{'pid': 12345, 'reaped': False, 'exit_code': None}])
            quarantine = report['media_quarantine']
            self.assertIn('could not be positively reaped', quarantine['reason'])
            self.assertEqual(set(quarantine['media']), {str(output / 'dist/probe.raw'),
                                                      str(output / 'dist/system7.qcow2'),
                                                      str(output / 'dist/bootrom.bin')})
            self.assertTrue(all(Path(path).is_file() for path in quarantine['media']))
            self.assertEqual((output / 'vm/pid').read_text(), '12345')
            self.assertTrue((output / 'vm/native-command.json').is_file())
            self.assertTrue(any('VM media quarantined' in error for error in report['cleanup_errors']))
            self.assertIn('private child stop failed', report['cleanup_errors'])

    def test_stop_error_after_positive_reap_still_prunes_only_private_media(self):
        child = Mock(pid=12345)
        child.poll.return_value = 0
        with tempfile.TemporaryDirectory() as folder:
            output, report, _ = self.capture_with_stop_failure(Path(folder), child)
            self.assertNotIn('media_quarantine', report)
            self.assertEqual(report['media_owners'], [{'pid': 12345, 'reaped': True, 'exit_code': 0}])
            self.assertEqual(list((output / 'dist').iterdir()), [])
            self.assertTrue((output / 'vm/native-command.json').is_file())
            self.assertEqual(report['cleanup_errors'], ['private child stop failed'])

    def test_missing_private_endpoint_never_targets_the_regular_vm_monitor(self):
        child = Mock()
        child.poll.return_value = None
        with patch.object(capture.vm, 'qmp') as qmp:
            capture.stop_owned(child, None)
        qmp.assert_not_called()
        child.terminate.assert_called_once_with()
        child.wait.assert_called_once_with(timeout=10)


if __name__ == '__main__':
    unittest.main()
