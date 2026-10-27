"""Failure cleanup for the isolated acceptance-only X11 owner."""
import io
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import private_x11_clipboard as x11


def causes(error):
    if isinstance(error, BaseExceptionGroup):
        return [cause for child in error.exceptions for cause in causes(child)]
    return [error]


def refused_library():
    library = MagicMock()
    library.XInitThreads.return_value = 1
    library.XOpenDisplay.return_value = 123
    library.XCreateSimpleWindow.return_value = 456
    library.XInternAtom.return_value = 789
    library.XGetSelectionOwner.return_value = 0
    return library


class PrivateX11CleanupTests(unittest.TestCase):
    def test_partial_peer_creation_never_destroys_a_null_xlib_handle(self):
        for display, window, message in [(0, 0, 'Cannot open'), (123, 0, 'could not create')]:
            with self.subTest(display=display):
                library = refused_library()
                library.XOpenDisplay.return_value = display
                library.XCreateSimpleWindow.return_value = window
                with patch.object(x11.c, 'CDLL', return_value=library), \
                     patch.object(x11.ctypes.util, 'find_library', return_value='private-test-xlib'):
                    with self.assertRaisesRegex(RuntimeError, message):
                        x11.ClipboardPeer('127.0.0.1:12345')
                library.XDestroyWindow.assert_not_called()
                if display:
                    library.XCloseDisplay.assert_called_once_with(display)
                else:
                    library.XCloseDisplay.assert_not_called()

    def test_refused_selection_releases_window_and_display_before_constructor_returns(self):
        library = refused_library()
        with patch.object(x11.c, 'CDLL', return_value=library), \
             patch.object(x11.ctypes.util, 'find_library', return_value='private-test-xlib'), \
             patch.object(x11.threading, 'Thread', side_effect=AssertionError('thread started after refusal')):
            with self.assertRaisesRegex(RuntimeError, 'selection ownership was refused'):
                x11.ClipboardPeer('127.0.0.1:12345')
        library.XDestroyWindow.assert_called_once_with(123, 456)
        library.XCloseDisplay.assert_called_once_with(123)

    def test_refusal_and_both_xlib_cleanup_errors_preserve_all_causes(self):
        library = refused_library()
        library.XDestroyWindow.side_effect = OSError('owned window destruction failed')
        library.XCloseDisplay.side_effect = OSError('owned display close failed')
        with patch.object(x11.c, 'CDLL', return_value=library), \
             patch.object(x11.ctypes.util, 'find_library', return_value='private-test-xlib'):
            with self.assertRaises(ExceptionGroup) as raised:
                x11.ClipboardPeer('127.0.0.1:12345')
        messages = [str(error) for error in causes(raised.exception)]
        self.assertTrue(any('selection ownership was refused' in message for message in messages))
        self.assertIn('owned window destruction failed', messages)
        self.assertIn('owned display close failed', messages)
        library.XCloseDisplay.assert_called_once_with(123)

    def test_failed_relay_connect_closes_both_untransferred_sockets(self):
        relay = x11.PrivateRelay.__new__(x11.PrivateRelay)
        relay.target = '\0/private-test-only'
        relay.stopping = threading.Event()
        relay.listener = Mock()
        relay.sockets = []
        relay.workers = []
        relay.errors = []
        client, server = Mock(), Mock()
        calls = 0
        def accept():
            nonlocal calls
            calls += 1
            if calls == 1:
                return client, ('127.0.0.1', 12345)
            relay.stopping.set()
            raise socket.timeout()
        relay.listener.accept.side_effect = accept
        server.connect.side_effect = OSError('private abstract socket connection refused')
        with patch.object(x11.socket, 'socket', return_value=server), \
             patch.object(x11.socket, 'AF_UNIX', 1, create=True):
            relay.accept()
        client.close.assert_called_once_with()
        server.close.assert_called_once_with()
        self.assertEqual(relay.workers, [])
        self.assertEqual(relay.errors, ['private abstract socket connection refused'])

    def test_failed_relay_bind_closes_its_listener(self):
        listener = Mock()
        listener.bind.side_effect = OSError('private loopback bind refused')
        with patch.object(x11.socket, 'socket', return_value=listener):
            with self.assertRaisesRegex(OSError, 'private loopback bind refused'):
                x11.PrivateRelay('123')
        listener.close.assert_called_once_with()

    def test_live_peer_thread_keeps_xlib_handles_but_parent_reaps_xvfb(self):
        peer = x11.ClipboardPeer.__new__(x11.ClipboardPeer)
        peer.stopping = threading.Event()
        peer.thread = Mock(ident=123)
        peer.thread.is_alive.return_value = True
        peer.lib = Mock()
        peer.display, peer.window = 123, 456
        owner = x11.PrivateX11.__new__(x11.PrivateX11)
        owner.peer = peer
        relay = owner.relay = Mock()
        owner.relay.close.side_effect = OSError('private relay shutdown failed')
        process = owner.process = Mock()
        process.poll.return_value = None
        owner.log = io.BytesIO()
        with self.assertRaises(ExceptionGroup) as raised:
            owner.close()
        messages = [str(error) for error in causes(raised.exception)]
        self.assertTrue(any('Xlib handles retained while its thread remains live' in message
                            for message in messages))
        self.assertIn('private relay shutdown failed', messages)
        peer.lib.XDestroyWindow.assert_not_called()
        peer.lib.XCloseDisplay.assert_not_called()
        self.assertEqual((peer.display, peer.window), (123, 456))
        relay.close.assert_called_once_with()
        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=5)
        self.assertIsNone(owner.process)
        self.assertTrue(owner.log.closed)

    def test_peer_failure_still_forces_and_reaps_stubborn_xvfb(self):
        owner = x11.PrivateX11.__new__(x11.PrivateX11)
        owner.peer = Mock()
        owner.peer.close.side_effect = RuntimeError('private peer shutdown failed')
        relay = owner.relay = Mock()
        process = owner.process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired('private-xvfb', 5), 0]
        owner.log = io.BytesIO()
        with self.assertRaisesRegex(ExceptionGroup, 'private peer shutdown failed'):
            owner.close()
        process.kill.assert_called_once_with()
        self.assertEqual(process.wait.call_count, 2)
        relay.close.assert_called_once_with()
        self.assertIsNone(owner.process)
        self.assertTrue(owner.log.closed)

    def test_constructor_preserves_startup_error_when_relay_cleanup_also_fails(self):
        process = Mock()
        process.poll.return_value = None
        relay = Mock(address='127.0.0.1:12345')
        relay.close.side_effect = OSError('private relay cleanup failed')
        with tempfile.TemporaryDirectory() as folder, \
             patch.object(x11.subprocess, 'Popen', return_value=process) as spawn, \
             patch.object(x11.select, 'select', return_value=([123], [], [])), \
             patch.object(x11.os, 'read', return_value=b'123\n'), \
             patch.object(x11, 'PrivateRelay', return_value=relay), \
             patch.object(x11, 'ClipboardPeer', side_effect=RuntimeError('private peer startup refused')):
            with self.assertRaises(ExceptionGroup) as raised:
                x11.PrivateX11('private-xvfb', Path(folder) / 'owned')
            self.assertTrue(spawn.call_args.kwargs['stdout'].closed)
        messages = [str(error) for error in causes(raised.exception)]
        self.assertIn('private peer startup refused', messages)
        self.assertIn('private relay cleanup failed', messages)
        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=5)

    def test_selection_reply_rejects_foreign_request_correlation(self):
        for field, unexpected in [('requestor', 99), ('selection', 99),
                                  ('target', 99), ('property', 99)]:
            with self.subTest(field=field):
                peer = x11.ClipboardPeer.__new__(x11.ClipboardPeer)
                peer.lib = Mock()
                peer.display, peer.window = 1, 2
                peer.clipboard, peer.utf8, peer.property = 3, 4, 5
                peer.errors = []
                peer.received = None
                peer.received_event = threading.Event()
                event = SimpleNamespace(requestor=2, selection=3, target=4, property=5)
                setattr(event, field, unexpected)
                peer.selection_notify(event)
                peer.lib.XGetWindowProperty.assert_not_called()
                self.assertIsNone(peer.received)
                self.assertTrue(peer.received_event.is_set())
                self.assertIn('does not match the private CLIPBOARD UTF8_STRING request', peer.errors[0])

    def test_selection_reply_requires_utf8_type_and_frees_refused_property(self):
        for actual_type, accepted in [(999, False), (4, True)]:
            with self.subTest(actual_type=actual_type):
                peer = x11.ClipboardPeer.__new__(x11.ClipboardPeer)
                peer.lib = Mock()
                peer.display, peer.window = 1, 2
                peer.clipboard, peer.utf8, peer.property = 3, 4, 5
                peer.errors = []
                peer.received = None
                peer.received_event = threading.Event()
                data = x11.c.create_string_buffer(b'private test')
                def property_value(*arguments):
                    arguments[7]._obj.value = actual_type
                    arguments[8]._obj.value = 8
                    arguments[9]._obj.value = len(b'private test')
                    arguments[10]._obj.value = 0
                    arguments[11]._obj.value = x11.c.addressof(data)
                    return 0
                peer.lib.XGetWindowProperty.side_effect = property_value
                peer.selection_notify(SimpleNamespace(requestor=2, selection=3, target=4, property=5))
                peer.lib.XFree.assert_called_once()
                self.assertTrue(peer.received_event.is_set())
                if accepted:
                    self.assertEqual(peer.received, b'private test')
                    self.assertEqual(peer.errors, [])
                else:
                    self.assertIsNone(peer.received)
                    self.assertIn('must have UTF8_STRING type', peer.errors[0])


if __name__ == '__main__':
    unittest.main()
