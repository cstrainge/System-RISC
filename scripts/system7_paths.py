"""Shared host build-output selection; guest and reference media stay separate."""
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BUILD_DIR_ENV = 'SYSTEM7_BUILD_DIR'


def build_dir():
    # Match mac_build::paths, including for subprocesses launched in System7/.
    return (ROOT / os.environ.get(BUILD_DIR_ENV, 'build/system7')).resolve()


def dist_dir():
    return build_dir() / 'dist'


def describe(path):
    """Reports accept output directories outside the outer workspace too."""
    path = Path(path)
    return str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path)
