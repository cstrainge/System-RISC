"""Launcher profile selection and protection of another VM's control endpoint."""
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import system7_vm as vm


class LauncherTests(unittest.TestCase):
    def test_invalid_bridge_is_rejected_before_touching_vm_storage_or_processes(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(vm.subprocess, 'Popen') as process:
            state = Path(folder) / 'new-vm'
            for port in [0, -1, 65536, True, '4444;anything']:
                with self.assertRaisesRegex(ValueError, 'loopback TCP port'):
                    vm.launch(state, host_bridge=port)
                self.assertFalse(state.exists())
            process.assert_not_called()

    def test_custom_build_directory_reaches_builder_without_shell_splitting(self):
        with tempfile.TemporaryDirectory(prefix='system risc ') as folder, \
             patch.dict(os.environ), patch.object(vm, 'DIST'), patch.object(vm, 'STATE'), \
             patch.object(sys, 'argv', ['system7_vm.py', 'build', '--build-dir', folder]), \
             patch.object(vm.subprocess, 'run') as run:
            vm.main()
            command = run.call_args.args[0]
            self.assertEqual(command, ['cargo', 'xtask', 'image', '--build-dir', folder])
            self.assertEqual(vm.DIST, Path(folder) / 'dist')
            self.assertEqual(vm.STATE, Path(folder) / 'vm')

    def test_check_defaults_to_separate_media(self):
        with patch.dict(os.environ), patch.object(vm, 'DIST'), patch.object(vm, 'STATE'), \
             patch.object(sys, 'argv', ['system7_vm.py', 'check', '--no-build']), \
             patch.object(vm, 'check') as check:
            os.environ.pop('SYSTEM7_BUILD_DIR', None)
            vm.main()
            self.assertEqual(vm.DIST, vm.ROOT / 'build/system7/check/dist')
            check.assert_called_once_with(False, None)

    def test_an_occupied_native_port_never_sends_commands_to_its_owner(self):
        with tempfile.TemporaryDirectory() as folder, \
             socket.socket() as listener, \
             patch.object(vm, 'native_executable', return_value=Path('fake-qemu.exe')), \
             patch.object(vm, 'qmp') as qmp:
            listener.bind(('127.0.0.1', 0))
            listener.listen()
            with self.assertRaisesRegex(RuntimeError, 'already in use'):
                vm.launch(Path(folder) / 'new-vm', 'native',
                          qmp_port=listener.getsockname()[1])
            qmp.assert_not_called()
            self.assertFalse((Path(folder) / 'new-vm/qmp-endpoint.json').exists())

    def test_check_keeps_separate_media_when_normal_root_comes_from_environment(self):
        with tempfile.TemporaryDirectory() as folder, \
             patch.dict(os.environ, {'SYSTEM7_BUILD_DIR': folder}), \
             patch.object(vm, 'DIST'), patch.object(vm, 'STATE'), \
             patch.object(sys, 'argv', ['system7_vm.py', 'check', '--no-build']), \
             patch.object(vm, 'check'):
            vm.main()
            self.assertEqual(vm.DIST, Path(folder) / 'check/dist')


if __name__ == '__main__':
    unittest.main()
