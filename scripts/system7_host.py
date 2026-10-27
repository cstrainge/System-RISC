"""Host process, locking and durability primitives used by VM orchestration."""
from contextlib import contextmanager
import ctypes
import os
from pathlib import Path


def process_options():
    if os.name == 'nt':
        import subprocess
        return {'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS}
    return {'start_new_session': True}


def process_exists(pid):
    if pid <= 0:
        return False
    if os.name != 'nt':
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        return True
    # os.kill(pid, 0) terminates processes on Windows. Query a process handle
    # instead. Windows can retain an exited process object whose OpenProcess
    # returns ACCESS_DENIED even though its PID is absent from the process list.
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.WaitForSingleObject.restype = wintypes.DWORD
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    handle = kernel.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
    if not handle:
        error = ctypes.get_last_error()
        if error == 87:  # ERROR_INVALID_PARAMETER: no such process
            return False
        if error == 5:
            return _windows_pid_present(kernel, pid)
        raise ctypes.WinError(error)
    try:
        result = kernel.WaitForSingleObject(handle, 0)
        if result == 0xFFFFFFFF:
            raise ctypes.WinError(ctypes.get_last_error())
        return result == 0x102  # WAIT_TIMEOUT: not exited
    finally:
        kernel.CloseHandle(handle)


def _windows_pid_present(kernel, pid):
    from ctypes import wintypes
    kernel.K32EnumProcesses.argtypes = [ctypes.POINTER(wintypes.DWORD), wintypes.DWORD,
                                       ctypes.POINTER(wintypes.DWORD)]
    kernel.K32EnumProcesses.restype = wintypes.BOOL
    capacity = 1024
    while True:
        processes = (wintypes.DWORD * capacity)()
        used = wintypes.DWORD()
        if not kernel.K32EnumProcesses(processes, ctypes.sizeof(processes), ctypes.byref(used)):
            raise ctypes.WinError(ctypes.get_last_error())
        # A full buffer may have omitted the PID. Grow before trusting absence.
        if used.value < ctypes.sizeof(processes):
            return pid in processes[:used.value // ctypes.sizeof(wintypes.DWORD)]
        capacity *= 2


@contextmanager
def exclusive_lock(path):
    # Keep the inode and lock byte stable: no unlink/recreate on release.
    with Path(path).open('a+b') as handle:
        try:
            if os.name == 'nt':
                import msvcrt
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise RuntimeError(f'Cannot acquire host lock {path}: {error}; another operation may be active, retry when it finishes') from error
        try:
            yield
        finally:
            if os.name == 'nt':
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)


def sync_file(path):
    # Windows' CRT commit requires a write-capable handle.
    with Path(path).open('r+b' if os.name == 'nt' else 'rb') as handle:
        os.fsync(handle.fileno())


def sync_directory(path):
    if os.name != 'nt':
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        return
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                  ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.FlushFileBuffers.argtypes = [wintypes.HANDLE]
    kernel.FlushFileBuffers.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    handle = kernel.CreateFileW(str(Path(path).resolve()), 0xC0000000, 7, None, 3,
                                0x02000000, None)  # read/write, share all, backup semantics
    if handle == wintypes.HANDLE(-1).value:
        error = ctypes.WinError(ctypes.get_last_error())
        error.filename = str(path)
        raise error
    try:
        if not kernel.FlushFileBuffers(handle):
            error = ctypes.WinError(ctypes.get_last_error())
            error.filename = str(path)
            raise error
    finally:
        kernel.CloseHandle(handle)
