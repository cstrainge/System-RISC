"""The build and VM wrappers must agree on the tracked platform selection."""
import os
from pathlib import Path
import sys
import tomllib
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import system7_platform as platform

class PlatformTests(unittest.TestCase):
    def test_default_cargo_profile_matches_shared_selection(self):
        config = tomllib.loads((Path(__file__).resolve().parents[2] / '.cargo/config.toml').read_text())
        self.assertEqual(config['target'][platform.TARGET]['rustflags'],
                         ['-C', 'target-feature=+' + platform.PLATFORM['llvm_profile']])
        self.assertEqual(platform.cpu_model(), 'rva23s64,mmu=on,pmp=on,sv39=on')

    def test_fixtures_outside_cargo_config_receive_the_same_isa(self):
        with patch.dict(os.environ, {'CARGO_ENCODED_RUSTFLAGS':'-C\x1fdebuginfo=0'}, clear=True):
            self.assertEqual(platform.guest_env()['CARGO_ENCODED_RUSTFLAGS'].split('\x1f'),
                             ['-C', 'debuginfo=0', '-C', 'target-feature=+rva23u64'])
            self.assertEqual(os.environ['CARGO_ENCODED_RUSTFLAGS'], '-C\x1fdebuginfo=0')

    def test_vector_test_property_is_architecturally_checked(self):
        self.assertEqual(platform.cpu_model(256), 'rva23s64,mmu=on,pmp=on,sv39=on,vlen=256')
        for value in [True, 0, 64, 129, 131072, '256']:
            with self.assertRaisesRegex(ValueError, 'power of two from 128 to 65536'):
                platform.cpu_model(value)

if __name__ == '__main__':
    unittest.main()
