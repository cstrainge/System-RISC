#!/usr/bin/env python3
"""Fail if a host test, a `#[cfg(test)]` item or a `tests/` crate comes back.

The project validates the operating system in the target runtime: native guest
applications driven by `cargo xtask boot-check`, not host-linked Rust tests that
substitute host allocation, scheduling and trap entry for the real ones. See
`docs/TESTING-POLICY.md`.

Two things are still legitimate and are not reported here:

* `#[cfg(not(test))]` and `cfg!(test)` guard production code and are left alone.
* Host tooling keeps its own tests under `tools/*/tests`, because that code runs
  on the developer machine rather than inside the guest.

`crates/mac-toolbox/tests/fixtures` and `crates/mac-toolbox/tests/data` are not
host tests: they are target applications and disk images that the boot cases
build and mount.
"""
import re
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[1]

# Directories that never contain project source: build output, generated media
# and the host capture results the checks write.
SKIP = {'target', 'dist', 'build', 'results', 'iso', '.git', 'vendor', 'node_modules',
        '__pycache__'}

# Guest test trees that must not exist, and the guest trees that are allowed.
FORBIDDEN_TEST_ROOTS = ('crates', 'drivers', 'system')
ALLOWED_TEST_DIRS = (Path('crates/mac-toolbox/tests/fixtures'),
                     Path('crates/mac-toolbox/tests/data'))
# The tests directory that holds those two is allowed, but only because they are
# all it holds: a Rust file or any other directory next to them is a host test.
ALLOWED_CONTAINER = Path('crates/mac-toolbox/tests')

# `#[cfg(...)]`, `#[cfg_attr(..., ...)]` and the inner `#![cfg(...)]` forms.
ATTRIBUTE = re.compile(r'#!?\[\s*(cfg|cfg_attr)\s*\(')
NOT_GROUP = re.compile(r'not\s*\([^()]*\)')


def strip_not(predicate):
    """Remove `not(...)` groups so `cfg(not(test))` does not look like a test."""
    previous = None
    while previous != predicate:
        previous = predicate
        predicate = NOT_GROUP.sub('', predicate)
    return predicate


def mentions_test(predicate):
    """True when `test` is a predicate term rather than part of a longer name.

    Hyphenated feature names such as `test-support` are not the `test` cfg, so a
    hyphen counts as part of the surrounding identifier here.
    """
    return re.search(r'(?<![\w:-])test(?![\w:-])', strip_not(predicate)) is not None


def attribute_body(text, start):
    """Yield the contents of the attribute whose opening paren ends at `start`."""
    depth = 1
    for index in range(start, len(text)):
        if text[index] == '(':
            depth += 1
        elif text[index] == ')':
            depth -= 1
            if depth == 0:
                return text[start:index]
    return text[start:]


def rust_offenders(path):
    """Report attributes in one Rust file that guard test-only code."""
    try:
        text = path.read_text(encoding='utf-8', errors='replace')
    except OSError as error:
        print(f'{path}: unreadable: {error}', file=sys.stderr)
        return []
    if 'test' not in text:
        return []
    found = []
    for match in ATTRIBUTE.finditer(text):
        body = attribute_body(text, match.end())
        if 'test' not in body:
            continue
        # `cfg_attr` carries its predicate first; `cfg_attr(predicate, attr...)`.
        mentions = mentions_test(body)
        if mentions:
            line = text.count('\n', 0, match.start()) + 1
            found.append(f'{path}:{line}: #[{match.group(1)}({body.strip()})]')
    return found


def walk(root):
    for entry in sorted(root.iterdir()):
        if entry.name in SKIP or entry.name.startswith('.'):
            continue
        if entry.is_dir():
            yield from walk(entry)
        else:
            yield entry


def find_test_directories():
    """Guest-side `tests/` trees that the policy forbids."""
    found = []
    for crate_root in FORBIDDEN_TEST_ROOTS:
        root = WORKSPACE / crate_root
        if not root.is_dir():
            continue
        for directory in sorted(path for path in root.rglob('tests') if path.is_dir()):
            if 'target' in directory.parts:
                continue
            relative = directory.relative_to(WORKSPACE)
            if any(relative == allowed or allowed in relative.parents
                   for allowed in ALLOWED_TEST_DIRS):
                continue
            if relative == ALLOWED_CONTAINER and all(
                    (WORKSPACE / relative / child.name).is_dir()
                    and relative / child.name in ALLOWED_TEST_DIRS
                    for child in directory.iterdir()):
                continue
            found.append(f'{relative}: host test crate directory')
    return found


def find_test_targets():
    """Explicit `[[test]]` sections in guest-side manifests."""
    found = []
    for manifest in sorted(WORKSPACE.rglob('Cargo.toml')):
        if any(part in SKIP for part in manifest.parts):
            continue
        relative = manifest.relative_to(WORKSPACE)
        if relative.parts[0] == 'tools' or relative.parts[0] == 'vendor':
            continue
        for number, line in enumerate(manifest.read_text(encoding='utf-8',
                                                         errors='replace').splitlines(), 1):
            if line.strip() == '[[test]]':
                found.append(f'{relative}:{number}: [[test]] target')
    return found


def main():
    offenders = find_test_directories() + find_test_targets()
    for path in walk(WORKSPACE):
        if path.suffix == '.rs':
            offenders.extend(rust_offenders(path.relative_to(WORKSPACE)))
    if offenders:
        print('Host tests are not allowed in this project '
              '(see docs/TESTING-POLICY.md):', file=sys.stderr)
        for offender in offenders:
            print(f'  {offender}', file=sys.stderr)
        print('Validate the target runtime with `cargo xtask boot-check --platform ...` '
              'and the native guest applications instead.', file=sys.stderr)
        return 1
    print('No host tests, test crates or #[cfg(test)] items found.')
    return 0


if __name__ == '__main__':
    sys.exit(main())
