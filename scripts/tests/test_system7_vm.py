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
import system7_paths as paths
import hfs_b_tool
import system7_exports


class LauncherTests(unittest.TestCase):
    def test_cocoa_zoom_requires_both_dimensions_to_fit_including_the_boundary(self):
        import json
        width, height = vm.display_size()
        for available_width, available_height, expected in [
                (width * 2, height * 2, 2),
                (width * 2 - 1, height * 2, 1),
                (width * 2, height * 2 - 1, 1),
                (width * 3, height * 3, 2),
                (width, height, 1)]:
            with self.subTest(size=(available_width, available_height)), \
                 patch.object(vm.subprocess, 'check_output', return_value=json.dumps({
                     'width': available_width, 'height': available_height})):
                self.assertEqual(vm.cocoa_scale(), expected)

    def test_cocoa_zoom_uses_the_selected_platform_geometry(self):
        with patch.object(vm, 'display_size', return_value=(1000, 700)), \
             patch.object(vm.subprocess, 'check_output', return_value='{"width":1920,"height":1080}'):
            self.assertEqual(vm.cocoa_scale(), 1)

    def test_cocoa_probe_failures_use_1x_and_explain_the_cause(self):
        import io
        import subprocess
        cases = [
            (None, OSError('screen probe missing'), 'screen probe missing'),
            (None, subprocess.CalledProcessError(1, 'osascript', stderr='No active Mac screen'), 'No active Mac screen'),
            (None, subprocess.TimeoutExpired('osascript', 10), 'timed out'),
            ('invalid JSON', None, 'Expecting value'),
            ('{"width":0,"height":1200}', None, 'finite positive width and height'),
            ('{"width":false,"height":1200}', None, 'finite positive width and height'),
            ('{"width":1600,"height":NaN}', None, 'finite positive width and height'),
            ('[]', None, 'finite positive width and height')]
        for output, error, cause in cases:
            with self.subTest(cause=cause), \
                 patch.object(vm.subprocess, 'check_output', return_value=output, side_effect=error), \
                 patch.object(vm.sys, 'stderr', new_callable=io.StringIO) as diagnostics:
                self.assertEqual(vm.cocoa_scale(), 1)
                self.assertIn('launching at 1x', diagnostics.getvalue())
                self.assertIn(cause, diagnostics.getvalue())

    def test_macos_start_selects_cocoa_without_windows_qemu_discovery(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as folder:
            state = Path(folder)
            (state / 'serial.log').write_text('DESKTOP_READY\n')
            with patch.object(vm, 'STATE', state), patch.object(vm.sys, 'platform', 'darwin'), \
                 patch.object(vm, 'qmp', side_effect=OSError), \
                 patch.object(vm, 'native_executable', side_effect=AssertionError('Windows discovery on Mac')), \
                 patch.object(vm, 'launch', return_value=(Mock(pid=456), {})) as launch:
                vm.start(rebuild=False, transfer=False)
            self.assertEqual(launch.call_args.args, (state, 'cocoa'))
            self.assertTrue(launch.call_args.kwargs['preserve'])

    def test_cocoa_launch_uses_native_display_and_preserves_host_environment(self):
        from unittest.mock import Mock
        import json
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            state = root / 'vm'
            dist = root / 'dist'
            dist.mkdir()
            for name in ['system7.qcow2', 'bootrom.bin']:
                (dist / name).write_bytes(b'launcher fixture')
            def spawn(*args, **kwargs):
                (state / 'serial.log').write_text('DESKTOP_READY\n')
                return Mock(pid=999999999, poll=lambda: None)
            with patch.object(vm, 'DIST', dist), patch.dict(os.environ, clear=True), \
                 patch.object(vm, 'cocoa_scale', return_value=2), \
                 patch.object(vm.subprocess, 'Popen', side_effect=spawn) as process, \
                 patch.object(vm, 'qmp', return_value={'status': 'running'}):
                vm.launch(state, 'cocoa')
            args = json.loads((state / 'command.json').read_text())
            self.assertEqual(args[0], 'qemu-system-riscv64')
            self.assertEqual(args[args.index('-display') + 1], 'cocoa,zoom-to-fit=off,scale=2')
            self.assertNotIn('GDK_BACKEND', process.call_args.kwargs['env'])
            self.assertNotIn('WSLENV', process.call_args.kwargs['env'])

    def test_cocoa_boot_error_reports_the_cause_and_reaps_its_guest(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            state = root / 'vm'
            dist = root / 'dist'
            dist.mkdir()
            for name in ['system7.qcow2', 'bootrom.bin']:
                (dist / name).write_bytes(b'launcher fixture')
            process = Mock(pid=999999999, poll=Mock(side_effect=[None, None, 0]))
            def spawn(args, **kwargs):
                (state / 'serial.log').write_text('SYSTEM_STARTUP_FAILED: startup resource unavailable\n')
                identity = args[args.index('-uuid') + 1]
                qmp.return_value = {'UUID': identity}
                return process
            with patch.object(vm, 'DIST', dist), patch.object(vm, 'cocoa_scale', return_value=1), \
                 patch.object(vm.subprocess, 'Popen', side_effect=spawn), \
                 patch.object(vm, 'qmp') as qmp:
                with self.assertRaisesRegex(RuntimeError, 'startup resource unavailable'):
                    vm.launch(state, 'cocoa')
            self.assertEqual([call.args[0] for call in qmp.call_args_list], ['query-uuid', 'quit'])
            process.wait.assert_called_once_with(timeout=10)

    def test_transfer_follows_the_profile_and_is_initialized_before_launch(self):
        for normal in [False, True]:
            with self.subTest(normal=normal), tempfile.TemporaryDirectory() as folder:
                state = Path(folder) / 'vm'
                selected = vm.ROOT / 'build/system7' if normal else Path(folder)
                expected = vm.SOURCE / 'transfer' if normal else selected / 'transfer'
                from unittest.mock import Mock
                companion = Mock(pid=123)
                process = Mock(pid=456)
                state.mkdir()
                (state / 'serial.log').write_text('DESKTOP_READY\n')
                with patch.object(vm, 'STATE', state), patch.object(vm, 'qmp', side_effect=OSError), \
                     patch.object(vm, 'build_dir', return_value=selected), \
                     patch.object(system7_exports, 'start', return_value=(companion, 1234)) as bridge, \
                     patch.object(vm, 'launch', return_value=(process, {})) as launch:
                    vm.start('headless', False)
                    bridge.assert_called_once_with(state, [f'Transfer={expected}'], (), initialize=expected)
                    self.assertEqual(launch.call_args.kwargs['expected_exports'], 1)
                    self.assertEqual(launch.call_args.kwargs['host_bridge'], 1234)

    def test_failed_vm_launch_reaps_its_own_companion(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as folder, patch.object(vm, 'STATE', Path(folder)), \
             patch.object(vm, 'qmp', side_effect=OSError), \
             patch.object(system7_exports, 'start', return_value=(Mock(pid=123), 1234)) as bridge, \
             patch.object(system7_exports, 'stop') as stop, \
             patch.object(vm, 'launch', side_effect=RuntimeError('failed boot')):
            with self.assertRaisesRegex(RuntimeError, 'failed boot'):
                vm.start('headless', False, exports=['Test=' + folder], transfer=False)
            stop.assert_called_once_with(bridge.return_value[0])

    def test_export_limit_is_checked_before_building_or_starting_anything(self):
        with patch.object(vm, 'build') as build, patch.object(vm, 'qmp') as qmp:
            with self.assertRaisesRegex(ValueError, 'sixteen'):
                vm.start(exports=['Unused=/tmp'] * 16)
            build.assert_not_called()
            qmp.assert_not_called()

    def test_exports_cannot_silently_change_a_running_vm(self):
        with patch.object(vm, 'qmp', return_value={}), patch.object(vm, 'build') as build:
            with self.assertRaisesRegex(RuntimeError, 'already running'):
                vm.start(exports=['Test=/tmp'])
            build.assert_not_called()

    def test_host_cache_and_hfs_tool_follow_the_selected_build_root(self):
        with tempfile.TemporaryDirectory(prefix='system risc ') as folder, \
             patch.dict(os.environ, {'SYSTEM7_BUILD_DIR': folder}), \
             patch.object(hfs_b_tool.subprocess, 'run') as run:
            hfs_b_tool.tool.cache_clear()
            try:
                expected = Path(folder).resolve() / 'host'
                self.assertEqual(paths.host_dir(), expected)
                self.assertEqual(hfs_b_tool.tool(), paths.host_tool('hfs-b'))
                command = run.call_args.args[0]
                self.assertEqual(command[-2:], ['--target-dir', str(expected)])
                self.assertEqual(run.call_args.kwargs['cwd'], vm.SOURCE)
            finally:
                hfs_b_tool.tool.cache_clear()

    def test_invalid_bridge_is_rejected_before_touching_vm_storage_or_processes(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(vm.subprocess, 'Popen') as process:
            state = Path(folder) / 'new-vm'
            for port in [0, -1, 65536, True, '4444;anything']:
                with self.assertRaisesRegex(ValueError, 'loopback TCP port'):
                    vm.launch(state, host_bridge=port)
                self.assertFalse(state.exists())
            process.assert_not_called()

    def test_unchanged_base_still_marks_stopped_desktop_disk_as_persistent(self):
        from unittest.mock import Mock
        import hashlib
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);state=root/'vm';dist=root/'dist'
            state.mkdir();dist.mkdir()
            for name in ['system7.qcow2','bootrom.bin']:
                (dist/name).write_bytes(b'current base')
            (state/'system7.qcow2').write_bytes(b'guest saved changes')
            (state/'base-disk.sha256').write_text(hashlib.sha256(b'current base').hexdigest())
            def start(*args,**kwargs):
                (state/'serial.log').write_text('DESKTOP_READY\n')
                return Mock(pid=999999999,poll=lambda:None)
            with patch.object(vm,'DIST',dist),patch.object(vm.subprocess,'Popen',side_effect=start), \
                 patch.object(vm, 'cocoa_scale', side_effect=AssertionError('screen probe in headless launch')), \
                 patch.object(vm,'qmp',return_value={'status':'running'}):
                vm.launch(state,preserve=True)
            self.assertEqual((state/'system7.qcow2').read_bytes(),b'guest saved changes')
            self.assertTrue((state/'persistent-user-state.json').exists())

    def test_failed_user_file_preservation_keeps_old_disk_rom_and_stamp(self):
        import system7_user_state
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            state = root / 'vm'
            dist = root / 'dist'
            state.mkdir();dist.mkdir()
            for name in ['system7.qcow2', 'bootrom.bin']:
                (state/name).write_bytes(b'old saved state')
                (dist/name).write_bytes(b'new release')
            (state/'base-disk.sha256').write_text('old hash')
            with patch.object(vm,'DIST',dist), patch.object(vm.subprocess,'Popen') as process, \
                 patch.object(system7_user_state,'replace_disk',side_effect=RuntimeError('Cannot export Preferences: read failed')):
                with self.assertRaisesRegex(RuntimeError,'Preferences: read failed'):
                    vm.launch(state,preserve=True)
                process.assert_not_called()
            self.assertEqual((state/'system7.qcow2').read_bytes(),b'old saved state')
            self.assertEqual((state/'bootrom.bin').read_bytes(),b'old saved state')
            self.assertEqual((state/'base-disk.sha256').read_text(),'old hash')

    def test_custom_build_directory_reaches_builder_without_shell_splitting(self):
        with tempfile.TemporaryDirectory(prefix='system risc ') as folder, \
             patch.dict(os.environ), patch.object(vm, 'DIST'), patch.object(vm, 'STATE'), \
             patch.object(sys, 'argv', ['system7_vm.py', 'build', '--build-dir', folder]), \
             patch.object(vm.subprocess, 'run') as run:
            vm.main()
            command = run.call_args.args[0]
            selected = Path(folder).resolve()
            self.assertEqual(command, ['cargo', 'xtask', 'image', '--build-dir', str(selected)])
            self.assertEqual(vm.DIST, selected / 'dist')
            self.assertEqual(vm.STATE, selected / 'vm')

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
            self.assertEqual(vm.DIST, Path(folder).resolve() / 'check/dist')


if __name__ == '__main__':
    unittest.main()
