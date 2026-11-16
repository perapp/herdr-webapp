#!/usr/bin/env python3
"""Offline checks for all non-Windows tagged-release targets; no CI/remote writes."""
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('release_ci', Path(__file__).with_name('release-ci.py'))
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)
EXPECTED = {
    'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
    'armv7-unknown-linux-gnueabihf', 'i686-unknown-linux-gnu',
    'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
    'x86_64-apple-darwin', 'aarch64-apple-darwin',
}


def executable(target):
    data = bytearray(32)
    arch = target.split('-')[0]
    if target.endswith('darwin'):
        data[:4] = b'\xcf\xfa\xed\xfe'
        struct.pack_into('<I', data, 4, 0x01000007 if arch == 'x86_64' else 0x0100000C)
    else:
        bits, machine = {'x86_64': (2, 62), 'aarch64': (2, 183), 'armv7': (1, 40), 'i686': (1, 3)}[arch]
        data[:6] = b'\x7fELF' + bytes([bits, 1])
        struct.pack_into('<H', data, 18, machine)
    return bytes(data)


class ReleaseCITests(unittest.TestCase):
    def setUp(self):
        # Mocked target execution must not masquerade as a real platform build.
        quiet = patch.object(ci, 'print', lambda *args, **kwargs: None, create=True)
        quiet.start()
        self.addCleanup(quiet.stop)

    def test_complete_matrix_excludes_windows_and_does_not_expand_main(self):
        rows = ci.matrix()
        self.assertEqual({row['target'] for row in rows}, EXPECTED)
        self.assertEqual(len(rows), 8)
        main = (ci.ROOT / '.github/workflows/main.yml').read_text()
        self.assertEqual(main.count('          - target:'), 3)
        for target in EXPECTED - {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin'}:
            self.assertNotIn(target, main)
        workflow = (ci.ROOT / '.github/workflows/release.yml').read_text()
        self.assertIn('fromJSON(needs.verify.outputs.matrix)', workflow)
        self.assertIn('release-ci.py matrix', workflow)
        self.assertIn('release-ci.py verify-binary', workflow)
        self.assertIn('release-ci.py assemble', workflow)
        self.assertIn('fetch-depth: 0', workflow)
        self.assertIn('--revision "$HERDR_BUILD_REVISION"', workflow)
        self.assertIn('needs: [verify, build]', workflow)
        self.assertNotIn('continue-on-error', workflow)

    def test_matrix_rejects_duplicates_unknown_targets_and_runners(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'targets.json'
            first = ci.matrix()[0]
            for rows in [[], [first, first], [{**first, 'target': 'x86_64-pc-windows-msvc'}], [{**first, 'runner': 'unknown'}], [{**first, 'execute': 'shell'}]]:
                with self.subTest(rows=rows):
                    path.write_text(json.dumps(rows))
                    with self.assertRaises(ValueError):
                        ci.matrix(path)

    def test_assembly_requires_every_target_and_checksums_all_eight_archives(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for target in EXPECTED:
                (root / f'herdr-webapp-v1.2.3-{target}.tar.gz').write_bytes(target.encode())
            ci.assemble(root, '1.2.3')
            sums = (root / 'SHA256SUMS').read_text().splitlines()
            self.assertEqual(len(sums), 9)
            for line in sums:
                digest, name = line.split('  ')
                self.assertEqual(digest, hashlib.sha256((root / name).read_bytes()).hexdigest())
            with self.assertRaises(ValueError):  # No stale/extra files accepted.
                ci.assemble(root, '1.2.3')
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                ci.assemble(Path(tmp), '1.2.3')

    def test_every_binary_executes_with_native_qemu_or_rosetta_and_checks_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / 'herdr-webapp'
            for row in ci.matrix():
                target = row['target']
                with self.subTest(target=target):
                    binary.write_bytes(executable(target))
                    calls = []
                    def run(args, **kwargs):
                        calls.append(args)
                        return '' if args[0] == 'readelf' else 'herdr-webapp 1.2.3\n'
                    with patch.object(ci.subprocess, 'check_output', side_effect=run):
                        ci.verify_binary(binary, target, '1.2.3')
                    invocation = calls[-1]
                    self.assertEqual(invocation[-2:], [str(binary.resolve()), '--version'])
                    if row['execute'].startswith('qemu-'):
                        self.assertEqual(invocation[:2], [row['execute'], '-L'])
                    if row['execute'] == 'rosetta':
                        self.assertEqual(invocation[:2], ['arch', '-x86_64'])
                    if target.endswith('musl'):
                        self.assertEqual(len(calls), 3)
                    wrong = 'i686-unknown-linux-gnu' if target.startswith('x86_64-') else 'x86_64-unknown-linux-gnu'
                    binary.write_bytes(executable(wrong))
                    with self.assertRaises(ValueError):
                        ci.verify_binary(binary, target, '1.2.3')

    def test_musl_cannot_publish_dynamic_or_wrong_version_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / 'herdr-webapp'
            binary.write_bytes(executable('x86_64-unknown-linux-musl'))
            for values in [['INTERP', ''], ['', ' (NEEDED) libc.so'], ['', '', 'herdr-webapp 9.9.9']]:
                with patch.object(ci.subprocess, 'check_output', side_effect=values):
                    with self.assertRaises(ValueError):
                        ci.verify_binary(binary, 'x86_64-unknown-linux-musl', '1.2.3')

    def test_clean_verified_tag_provenance_is_required_before_packaging(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / 'herdr-webapp'
            binary.write_bytes(executable('x86_64-unknown-linux-gnu'))
            revision = 'a' * 40
            # Compile the real shared metadata formatter, rather than inventing
            # the tagged display format in a mock (it strips the conventional v).
            source = Path(tmp) / 'metadata.rs'
            formatter = Path(tmp) / 'metadata'
            module = json.dumps(str(ci.ROOT / 'src/build_info/validation.rs'))
            source.write_text(f'mod validation {{ include!({module}); }}\nfn main() {{ println!("herdr-webapp {{}}", validation::display_version(None, None, Some("v1.2.3")).unwrap()); }}\n')
            ci.subprocess.run(['rustc', '--edition=2024', str(source), '-o', str(formatter)], check=True)
            display = ci.subprocess.check_output([str(formatter)], text=True).strip()
            with patch.object(ci.subprocess, 'check_output', side_effect=['herdr-webapp 1.2.3', revision, display]):
                ci.verify_binary(binary, 'x86_64-unknown-linux-gnu', '1.2.3', revision)
            for sha, display in [('b' * 40, 'herdr-webapp 1.2.3'), (revision, 'herdr-webapp untagged')]:
                with patch.object(ci.subprocess, 'check_output', side_effect=['herdr-webapp 1.2.3', sha, display]):
                    with self.assertRaises(ValueError):
                        ci.verify_binary(binary, 'x86_64-unknown-linux-gnu', '1.2.3', revision)


if __name__ == '__main__':
    unittest.main()
