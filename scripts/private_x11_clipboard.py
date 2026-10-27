"""Private acceptance-only Xvfb server and UTF-8 X11 selection peer.

Never opens the caller's DISPLAY. Every connection uses the displayfd result
from the fresh owned Xvfb process; TCP listening is disabled.
"""
import ctypes as c
import ctypes.util
import errno
import os
from pathlib import Path
import select
import socket
import subprocess
import threading

HOST_TEXT = 'Private host café 🦀\nsecond line'
GUEST_TEXT = 'Private guest 雪 🦀\nsecond line'
Atom = Window = c.c_ulong


def attempt(errors, action):
    try:
        action()
    except Exception as error:
        errors.append(error)


def cleanup_failed(owner, errors):
    if errors:
        raise ExceptionGroup(f'{owner} cleanup failed: {errors[0]}', errors)


class SelectionRequest(c.Structure):
    _fields_ = [('type', c.c_int), ('serial', c.c_ulong), ('send_event', c.c_int),
                ('display', c.c_void_p), ('owner', Window), ('requestor', Window),
                ('selection', Atom), ('target', Atom), ('property', Atom), ('time', c.c_ulong)]


class SelectionNotify(c.Structure):
    _fields_ = [('type', c.c_int), ('serial', c.c_ulong), ('send_event', c.c_int),
                ('display', c.c_void_p), ('requestor', Window), ('selection', Atom),
                ('target', Atom), ('property', Atom), ('time', c.c_ulong)]


class Event(c.Union):
    _fields_ = [('type', c.c_int), ('request', SelectionRequest),
                ('notify', SelectionNotify), ('pad', c.c_long * 24)]


class PrivateRelay:
    """Loopback-only bridge to our owned abstract socket, never WSLg's path.

    Current Xlib deliberately declines abstract-client fallback, while the WSLg
    filesystem socket directory is mounted read-only. Xvfb itself keeps TCP
    disabled; only this owned ephemeral 127.0.0.1 listener is exposed.
    """
    def __init__(self, number):
        self.target = '\0/tmp/.X11-unix/X' + number
        self.listener = socket.socket()
        self.stopping = threading.Event()
        self.sockets = []
        self.workers = []
        self.errors = []
        try:
            self.listener.bind(('127.0.0.1', 0))
            self.listener.listen()
            self.listener.settimeout(.1)
            port = self.listener.getsockname()[1]
            if port < 6000:
                raise RuntimeError('Private X11 relay requires an ephemeral port above 6000')
            self.address = '127.0.0.1:' + str(port - 6000)
            self.thread = threading.Thread(target=self.accept, daemon=True)
            self.thread.start()
        except Exception as error:
            errors = [error]
            attempt(errors, self.listener.close)
            if len(errors) > 1:
                raise ExceptionGroup('Private X11 relay startup and cleanup failed', errors) from error
            raise

    def accept(self):
        while not self.stopping.is_set():
            client = server = None
            try:
                client, _ = self.listener.accept()
                server = socket.socket(socket.AF_UNIX)
                server.settimeout(1)
                server.connect(self.target)
                client.settimeout(1)
                server.settimeout(1)
                self.sockets.extend((client, server))
                worker = threading.Thread(target=self.bridge, args=(client, server), daemon=True)
                worker.start()
                self.workers.append(worker)
                # The worker now owns both sockets, including their close.
                client = server = None
            except socket.timeout as error:
                if client is not None and not self.stopping.is_set():
                    self.errors.append(str(error))
            except Exception as error:
                if not self.stopping.is_set():
                    self.errors.append(str(error))
            finally:
                errors = []
                for stream in (client, server):
                    if stream is not None:
                        attempt(errors, stream.close)
                self.errors.extend(str(error) for error in errors)

    def bridge(self, client, server):
        try:
            while not self.stopping.is_set():
                ready, _, _ = select.select([client, server], [], [], .1)
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    (server if source is client else client).sendall(data)
        except (OSError, ValueError) as error:
            if not self.stopping.is_set():
                self.errors.append(str(error))
        finally:
            errors = []
            attempt(errors, client.close)
            attempt(errors, server.close)
            self.errors.extend(str(error) for error in errors)

    def close(self):
        self.stopping.set()
        errors = []
        attempt(errors, self.listener.close)
        attempt(errors, lambda: self.thread.join(timeout=3))
        for stream in self.sockets:
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError as error:
                if error.errno not in (errno.EBADF, errno.ENOTCONN):
                    errors.append(error)
            attempt(errors, stream.close)
        for worker in self.workers:
            attempt(errors, lambda: worker.join(timeout=3))
        if self.thread.is_alive() or any(worker.is_alive() for worker in self.workers):
            errors.append(RuntimeError('Private X11 relay did not acknowledge shutdown'))
        cleanup_failed('Private X11 relay', errors)


class ClipboardPeer:
    def __init__(self, address):
        self.display = None
        self.window = None
        self.thread = None
        self.stopping = threading.Event()
        try:
            self.initialize(address)
        except Exception as error:
            errors = [error]
            attempt(errors, self.close)
            if len(errors) > 1:
                raise ExceptionGroup('Private X11 peer startup and cleanup failed', errors) from error
            raise

    def initialize(self, address):
        self.lib = c.CDLL(ctypes.util.find_library('X11') or 'libX11.so.6')
        signatures = {
            'XInitThreads': (c.c_int, []),
            'XOpenDisplay': (c.c_void_p, [c.c_char_p]),
            'XDefaultRootWindow': (Window, [c.c_void_p]),
            'XCreateSimpleWindow': (Window, [c.c_void_p, Window, c.c_int, c.c_int, c.c_uint, c.c_uint, c.c_uint, c.c_ulong, c.c_ulong]),
            'XInternAtom': (Atom, [c.c_void_p, c.c_char_p, c.c_int]),
            'XSetSelectionOwner': (c.c_int, [c.c_void_p, Atom, Window, c.c_ulong]),
            'XGetSelectionOwner': (Window, [c.c_void_p, Atom]),
            'XPending': (c.c_int, [c.c_void_p]),
            'XNextEvent': (c.c_int, [c.c_void_p, c.POINTER(Event)]),
            'XChangeProperty': (c.c_int, [c.c_void_p, Window, Atom, Atom, c.c_int, c.c_int, c.c_void_p, c.c_int]),
            'XSendEvent': (c.c_int, [c.c_void_p, Window, c.c_int, c.c_long, c.POINTER(Event)]),
            'XConvertSelection': (c.c_int, [c.c_void_p, Atom, Atom, Atom, Window, c.c_ulong]),
            'XGetWindowProperty': (c.c_int, [c.c_void_p, Window, Atom, c.c_long, c.c_long, c.c_int, Atom, c.POINTER(Atom), c.POINTER(c.c_int), c.POINTER(c.c_ulong), c.POINTER(c.c_ulong), c.POINTER(c.c_void_p)]),
            'XFree': (c.c_int, [c.c_void_p]),
            'XFlush': (c.c_int, [c.c_void_p]),
            'XDestroyWindow': (c.c_int, [c.c_void_p, Window]),
            'XCloseDisplay': (c.c_int, [c.c_void_p])}
        for name, (result, arguments) in signatures.items():
            function = getattr(self.lib, name)
            function.restype = result
            function.argtypes = arguments
        if not self.lib.XInitThreads():
            raise RuntimeError('Private X11 peer could not initialize Xlib synchronization')
        self.display = self.lib.XOpenDisplay(address.encode('ascii'))
        if not self.display:
            self.display = None
            raise RuntimeError('Cannot open the newly owned private Xvfb display')
        self.window = self.lib.XCreateSimpleWindow(self.display, self.lib.XDefaultRootWindow(self.display), 0, 0, 1, 1, 0, 0, 0)
        if not self.window:
            self.window = None
            raise RuntimeError('Private X11 peer could not create its owned selection window')
        self.clipboard = self.atom('CLIPBOARD')
        self.utf8 = self.atom('UTF8_STRING')
        self.targets = self.atom('TARGETS')
        self.text = self.atom('TEXT')
        self.property = self.atom('SYSTEM_RISC_PRIVATE_TEST')
        self.lib.XSetSelectionOwner(self.display, self.clipboard, self.window, 0)
        self.lib.XFlush(self.display)
        if self.lib.XGetSelectionOwner(self.display, self.clipboard) != self.window:
            raise RuntimeError('Private X11 clipboard selection ownership was refused')
        self.received_event = threading.Event()
        self.received = None
        self.served_host_utf8 = 0
        self.target_requests = 0
        self.errors = []
        self.ready_file = None
        self.reoffered = False
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def atom(self, name):
        return self.lib.XInternAtom(self.display, name.encode('ascii'), False)

    def selection_request(self, request):
        target = request.target
        property = request.property or target
        reply = Event()
        reply.notify = SelectionNotify(31, 0, True, self.display, request.requestor,
                                       request.selection, target, 0, request.time)
        if request.selection == self.clipboard and target == self.targets:
            self.target_requests += 1
            atoms = (Atom * 3)(self.targets, self.utf8, self.text)
            self.lib.XChangeProperty(self.display, request.requestor, property, 4, 32, 0, c.cast(atoms, c.c_void_p), 3)
            reply.notify.property = property
        elif request.selection == self.clipboard and target in (self.utf8, self.text):
            data = HOST_TEXT.encode('utf-8')
            storage = c.create_string_buffer(data)
            self.lib.XChangeProperty(self.display, request.requestor, property, self.utf8, 8, 0, c.cast(storage, c.c_void_p), len(data))
            self.served_host_utf8 += 1
            reply.notify.property = property
        self.lib.XSendEvent(self.display, request.requestor, False, 0, c.byref(reply))
        self.lib.XFlush(self.display)

    def selection_notify(self, event):
        if (event.requestor != self.window or event.selection != self.clipboard
                or event.target != self.utf8 or event.property not in (0, self.property)):
            self.errors.append('Private GTK selection reply does not match the private CLIPBOARD '
                               'UTF8_STRING request')
            self.received_event.set()
            return
        if event.property == 0:
            self.errors.append('Private GTK peer refused the UTF-8 selection request')
            self.received_event.set()
            return
        actual_type = Atom()
        actual_format = c.c_int()
        count = c.c_ulong()
        remaining = c.c_ulong()
        data = c.c_void_p()
        status = self.lib.XGetWindowProperty(self.display, self.window, event.property, 0, 16384, True, 0,
                                            c.byref(actual_type), c.byref(actual_format), c.byref(count), c.byref(remaining), c.byref(data))
        try:
            if (status != 0 or actual_type.value != self.utf8
                    or actual_format.value != 8 or remaining.value != 0):
                self.errors.append('Private GTK property must have UTF8_STRING type, 8-bit format '
                                   'and a complete bounded value')
            else:
                self.received = c.string_at(data, count.value)
        finally:
            if data:
                self.lib.XFree(data)
        self.received_event.set()

    def serve(self):
        requested_owner = self.window
        try:
            while not self.stopping.is_set():
                if self.ready_file is not None and not self.reoffered and self.ready_file.is_file():
                    if 'VDAGENT_NATIVE_CAPABILITIES_PASS ' in self.ready_file.read_text(errors='replace'):
                        # GTK subscribes after its window is initialized. Make
                        # one real private ownership change after negotiation,
                        # rather than assuming it observes preexisting owners.
                        self.lib.XSetSelectionOwner(self.display, self.clipboard, 0, 0)
                        self.lib.XSetSelectionOwner(self.display, self.clipboard, self.window, 0)
                        self.lib.XFlush(self.display)
                        self.reoffered = True
                while self.lib.XPending(self.display):
                    event = Event()
                    self.lib.XNextEvent(self.display, c.byref(event))
                    if event.type == 30:
                        self.selection_request(event.request)
                    elif event.type == 31:
                        self.selection_notify(event.notify)
                owner = self.lib.XGetSelectionOwner(self.display, self.clipboard)
                if owner and owner != self.window and owner != requested_owner:
                    self.lib.XConvertSelection(self.display, self.clipboard, self.utf8, self.property, self.window, 0)
                    self.lib.XFlush(self.display)
                    requested_owner = owner
                self.stopping.wait(.01)
        except Exception as error:
            self.errors.append(str(error))
            self.received_event.set()

    def close(self):
        self.stopping.set()
        if self.thread is not None:
            if self.thread.ident is not None:
                self.thread.join(timeout=3)
            if self.thread.is_alive():
                # Retain Xlib handles while its owning thread can still use
                # them. PrivateX11 still shuts down its relay and Xvfb.
                raise RuntimeError('Private X11 selection peer did not acknowledge shutdown; '
                                   'Xlib handles retained while its thread remains live')
        errors = []
        if self.window is not None:
            try:
                self.lib.XDestroyWindow(self.display, self.window)
                self.window = None
            except Exception as error:
                errors.append(error)
        if self.display is not None:
            try:
                self.lib.XCloseDisplay(self.display)
                self.display = None
                self.window = None
            except Exception as error:
                errors.append(error)
        cleanup_failed('Private X11 selection peer', errors)


class PrivateX11:
    def __init__(self, executable, output):
        self.output = Path(output)
        self.output.mkdir()
        self.log = (self.output / 'xvfb.log').open('wb')
        read_fd = write_fd = None
        selected = dict(os.environ)
        for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS'):
            selected.pop(name, None)
        self.process = None
        self.peer = None
        self.relay = None
        try:
            read_fd, write_fd = os.pipe()
            self.process = subprocess.Popen([str(executable), '-displayfd', str(write_fd), '-nolisten', 'tcp',
                                             '-nolisten', 'unix', '-listen', 'local', '-ac',
                                             '-screen', '0', '1024x768x24', '-noreset'],
                                            pass_fds=(write_fd,), env=selected, stdout=self.log, stderr=self.log)
            os.close(write_fd)
            write_fd = None
            if not select.select([read_fd], [], [], 10)[0]:
                raise RuntimeError('Private Xvfb did not return an owned display number: '
                                   + (self.output / 'xvfb.log').read_text(errors='replace')[-2000:])
            number = os.read(read_fd, 128).decode('ascii').strip()
            if not number.isdigit() or self.process.poll() is not None:
                raise RuntimeError('Private Xvfb failed: ' + (self.output / 'xvfb.log').read_text(errors='replace'))
            self.relay = PrivateRelay(number)
            self.address = self.relay.address
            self.peer = ClipboardPeer(self.address)
        except Exception as error:
            errors = [error]
            attempt(errors, self.close)
            if len(errors) > 1:
                raise ExceptionGroup('Private X11 startup and cleanup failed', errors) from error
            raise
        finally:
            if read_fd is not None:
                os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)

    def environment(self):
        return {'DISPLAY': self.address, 'GDK_BACKEND': 'x11', 'GSETTINGS_BACKEND': 'memory',
                'XDG_CONFIG_HOME': str(self.output / 'config'), 'XDG_CACHE_HOME': str(self.output / 'cache')}

    def close(self):
        errors = []
        if self.peer is not None:
            try:
                self.peer.close()
                self.peer = None
            except Exception as error:
                errors.append(error)
        if self.relay is not None:
            try:
                self.relay.close()
                self.relay = None
            except Exception as error:
                errors.append(error)
        if self.process is not None:
            try:
                if self.process.poll() is None:
                    attempt(errors, self.process.terminate)
            except Exception as error:
                errors.append(error)
            try:
                self.process.wait(timeout=5)
                self.process = None
            except subprocess.TimeoutExpired:
                attempt(errors, self.process.kill)
                try:
                    self.process.wait(timeout=5)
                    self.process = None
                except Exception as error:
                    errors.append(error)
            except Exception as error:
                errors.append(error)
        attempt(errors, self.log.close)
        cleanup_failed('Private X11', errors)
