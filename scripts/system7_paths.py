"""Shared host build-output selection; guest and reference media stay separate."""
import os
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[1]
ROOT = SOURCE.parent
BUILD_DIR_ENV = 'SYSTEM7_BUILD_DIR'
DIST_ENV = 'SYSTEM7_DIST'
PLATFORM_ENV = 'SYSTEM7_PLATFORM'
DEFAULT_PLATFORM = 'qemu_riscv'


def build_dir():
    # Match mac_build::paths, including for subprocesses launched in System7/.
    return (ROOT / os.environ.get(BUILD_DIR_ENV, 'build/system7')).resolve()


def dist_dir():
    """The selected platform's output directory.

    `xtask` owns this path and exports it, so scripts never re-derive the
    platform segment themselves; the fallback only serves a hand-run script
    after a default build.
    """
    chosen = os.environ.get(DIST_ENV)
    if chosen:
        path = Path(chosen)
        return (path if path.is_absolute() else build_dir() / path).resolve()
    return (build_dir() / 'dist' / os.environ.get(PLATFORM_ENV, DEFAULT_PLATFORM)).resolve()


def host_dir():
    """One compiler cache for host tools/examples within the selected build."""
    return build_dir() / 'host'


def host_tool(name, profile='debug'):
    return host_dir() / profile / (name + ('.exe' if os.name == 'nt' else ''))


def describe(path):
    """Reports accept output directories outside the outer workspace too."""
    path = Path(path)
    return str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path)
