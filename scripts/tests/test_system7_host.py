"""Real host primitives: liveness must not kill, locks must exclude and release."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from system7_host import exclusive_lock, process_exists, sync_directory, sync_file
from system7_exports import readiness, stop


class HostTests(unittest.TestCase):
    @unittest.skipUnless(os.name == 'nt', 'Windows process enumeration')
    def test_access_denied_pid_requires_an_active_process(self):
        import ctypes
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        denied = Mock()
        denied.OpenProcess.return_value = None
        denied.K32EnumProcesses = kernel.K32EnumProcesses
        child = subprocess.Popen([sys.executable, '-c', 'pass'])
        child.wait(timeout=5)
        with patch.object(ctypes, 'WinDLL', return_value=denied), \
                patch.object(ctypes, 'get_last_error', return_value=5):
            self.assertTrue(process_exists(os.getpid()))
            self.assertFalse(process_exists(child.pid))
            denied.WaitForSingleObject.assert_not_called()

    def test_process_list_grows_before_reporting_a_missing_pid(self):
        import ctypes
        from ctypes import wintypes
        from system7_host import _windows_pid_present
        kernel = Mock()
        def enumerate_processes(processes, size, used):
            if len(processes) == 1024:
                used._obj.value = size
            else:
                processes[0] = 123
                used._obj.value = ctypes.sizeof(wintypes.DWORD)
            return True
        kernel.K32EnumProcesses.side_effect = enumerate_processes
        self.assertTrue(_windows_pid_present(kernel, 123))
        self.assertFalse(_windows_pid_present(kernel, 456))
        self.assertEqual(kernel.K32EnumProcesses.call_count, 4)

    @unittest.skipUnless(os.name == 'nt', 'Windows process enumeration')
    def test_process_enumeration_failure_reports_ownership_uncertainty(self):
        import ctypes
        from system7_host import _windows_pid_present
        kernel = Mock()
        kernel.K32EnumProcesses.return_value = False
        with patch.object(ctypes, 'get_last_error', return_value=5):
            with self.assertRaisesRegex(OSError, 'Access is denied'):
                _windows_pid_present(kernel, 123)

    def test_liveness_does_not_signal_a_process_and_detects_its_exit(self):
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            self.assertTrue(process_exists(child.pid))
            self.assertIsNone(child.poll(), 'liveness probe terminated its process')
            self.assertTrue(process_exists(os.getpid()))
        finally:
            child.terminate()
            child.wait(timeout=5)
        self.assertFalse(process_exists(child.pid))
        self.assertFalse(process_exists(0))

    def test_lock_excludes_other_processes_and_releases_after_failure(self):
        source = str(Path(__file__).resolve().parents[1])
        script = '''import sys
sys.stdout.reconfigure(encoding='utf-8')
sys.path.insert(0, sys.argv[1])
from system7_host import exclusive_lock
try:
    with exclusive_lock(sys.argv[2]):
        print('acquired')
except RuntimeError as error:
    print(error)
    sys.exit(7)
'''
        with tempfile.TemporaryDirectory(prefix='System RISC lock ') as folder:
            path = Path(folder) / 'lock'
            command = [sys.executable, '-c', script, source, str(path)]
            with self.assertRaisesRegex(ValueError, 'simulated operation failure'):
                with exclusive_lock(path):
                    result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8')
                    self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
                    self.assertIn(str(path), result.stdout)
                    self.assertIn('retry', result.stdout)
                    raise ValueError('simulated operation failure')
            result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(path.exists(), 'release unlinked the shared lock inode')

    def test_flushes_are_real_and_missing_directories_are_errors(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder)
            data = path / 'data'
            data.write_bytes(b'durable bytes')
            sync_file(data)
            sync_directory(path)
            with self.assertRaisesRegex(OSError, 'missing'):
                sync_directory(path / 'missing')

    def test_companion_readiness_handles_pipes_errors_timeout_and_retry(self):
        with tempfile.TemporaryDirectory() as folder:
            log = Path(folder) / 'companion.log'
            for code, message in [
                ("import sys; print('failed export: access denied',file=sys.stderr,flush=True)", 'access denied'),
                ("print('HOST_BRIDGE_READY 127.0.0.1:not-a-port',flush=True)", 'invalid companion loopback port'),
                ("import time; time.sleep(30)", 'did not become ready')]:
                with self.subTest(message=message), log.open('wb') as output:
                    child = subprocess.Popen([sys.executable, '-c', code], stdout=subprocess.PIPE, stderr=output)
                    try:
                        timeout = .1 if 'time.sleep' in code else 5
                        with self.assertRaisesRegex((RuntimeError, TimeoutError), message):
                            readiness(child, log, timeout=timeout)
                    finally:
                        stop(child)
                    self.assertIsNotNone(child.returncode, 'failed startup left its companion alive')
            with log.open('wb') as output:
                child = subprocess.Popen([sys.executable, '-c',
                    "print('HOST_BRIDGE_READY 127.0.0.1:12345',flush=True)"],
                    stdout=subprocess.PIPE, stderr=output)
                try:
                    self.assertEqual(readiness(child, log), 12345)
                finally:
                    stop(child)


if __name__ == '__main__':
    unittest.main()
