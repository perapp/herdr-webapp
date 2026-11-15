#!/usr/bin/env python3
"""Offline publisher tests; stdlib only, no GitHub credentials or live core."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('main_ci', Path(__file__).with_name('main-ci.py'))
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)
REVISION = 'a' * 40
VERSION = '0.0.8'
RUN_ID = 123


def executable(target):
    data = bytearray(128)
    if target == ci.TARGETS[2]:
        data[:4] = b'\xcf\xfa\xed\xfe'
        struct.pack_into('<I', data, 4, 0x0100000C)
        struct.pack_into('<I', data, 12, 2)
    else:
        data[:7] = b'\x7fELF\x02\x01\x01'
        struct.pack_into('<H', data, 16, 3)
        struct.pack_into('<H', data, 18, 62 if target == ci.TARGETS[0] else 183)
    return bytes(data)


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='main-ci-test-')
        self.addCleanup(self.temporary.cleanup)
        # /var on macOS is a symlink; canonicalize the fixture root, not inputs.
        self.root = Path(self.temporary.name).resolve()
        self.inputs = self.root / 'inputs'
        self.inputs.mkdir()
        self.output = self.root / 'site'
        for target in ci.TARGETS:
            directory = self.inputs / ('main-native-' + target)
            directory.mkdir()
            data = executable(target)
            (directory / 'herdr-webapp').write_bytes(data)
            metadata = dict(schemaVersion=1, repository=ci.REPOSITORY, workflow=ci.WORKFLOW,
                            runId=RUN_ID, revision=REVISION, version=VERSION, target=target,
                            hostSystem=ci.HOSTS[target][0], hostMachine=ci.HOSTS[target][1],
                            size=len(data), sha256=hashlib.sha256(data).hexdigest())
            (directory / 'native.json').write_bytes(ci.json_bytes(metadata))
        self.first = self.inputs / ('main-native-' + ci.TARGETS[0])

    def assemble(self, **kwargs):
        return ci.assemble(self.inputs, self.output, kwargs.get('revision', REVISION),
                           kwargs.get('version', VERSION), kwargs.get('run_id', RUN_ID))

    def rejected(self, **kwargs):
        with self.assertRaises((ci.InvalidBuild, ValueError, OSError)):
            self.assemble(**kwargs)
        self.assertFalse(self.output.exists())
        self.assertFalse(list(self.root.glob('.main-ci-*')))

    def mutate(self, **changes):
        path = self.first / 'native.json'
        value = json.loads(path.read_bytes())
        value.update(changes)
        path.write_bytes(ci.json_bytes(value))

    def test_complete_site_exact_schema_and_bytes(self):
        self.assemble()
        manifest_bytes = (self.output / 'main/client/manifest.json').read_bytes()
        manifest = json.loads(manifest_bytes)
        self.assertEqual(set(manifest), {'schemaVersion', 'protocolVersion', 'channel', 'version', 'revision', 'assets'})
        self.assertEqual({k: v for k, v in manifest.items() if k != 'assets'}, dict(
            schemaVersion=2, protocolVersion=1, channel='main', version=VERSION, revision=REVISION))
        self.assertEqual([a['target'] for a in manifest['assets']], list(ci.TARGETS))
        expected_files = {'main/client/manifest.json', 'main/publication.json'}
        for asset in manifest['assets']:
            self.assertEqual(set(asset), {'target', 'url', 'size', 'sha256'})
            relative = f'main/client/{REVISION}/{asset["target"]}/herdr-webapp'
            self.assertEqual(asset['url'], '/' + relative)
            data = (self.output / relative).read_bytes()
            self.assertEqual(data, executable(asset['target']))
            self.assertEqual(asset['size'], len(data))
            self.assertEqual(asset['sha256'], hashlib.sha256(data).hexdigest())
            expected_files.add(relative)
        self.assertEqual({str(p.relative_to(self.output)) for p in self.output.rglob('*') if p.is_file()}, expected_files)
        self.assertEqual(json.loads((self.output / 'main/publication.json').read_bytes()), dict(
            schemaVersion=1, repository=ci.REPOSITORY, branch='main', revision=REVISION,
            workflow=ci.WORKFLOW, runId=RUN_ID, manifestSha256=hashlib.sha256(manifest_bytes).hexdigest()))

    def test_missing_target(self):
        import shutil
        shutil.rmtree(self.first)
        self.rejected()

    def test_extra_target_or_file(self):
        for name in ('main-native-extra', 'surprise'):
            path = self.inputs / name
            path.mkdir()
            self.rejected()
            path.rmdir()
        (self.first / 'extra').write_bytes(b'x')
        self.rejected()

    def test_missing_binary(self):
        (self.first / 'herdr-webapp').unlink()
        self.rejected()

    def test_invalid_context(self):
        for revision in ('a' * 7, 'A' * 40, '../' + 'a' * 40, 'a' * 64, ''):
            with self.subTest(revision=revision):
                self.rejected(revision=revision)
        for version in ('v0.0.8', '00.0.8', '0.0', '0.0.8\n', '../escape', '0.0.8-01', '0.0.8-a..b', '0.0.8+foo..bar', '1.2.3-rc.0', '1.2.3+build.001', '1' * 65 + '.0.0'):
            with self.subTest(version=version):
                self.rejected(version=version)
        for run_id in (0, -1, True, '123'):
            with self.subTest(run_id=run_id):
                self.rejected(run_id=run_id)

    def test_valid_catalog_version_context(self):
        for version in ('0.0.0', '1.2.3', '123.456.789'):
            ci.context(REVISION, version, RUN_ID)

    def test_provenance_and_digest_mismatch(self):
        original = (self.first / 'native.json').read_bytes()
        changes = dict(schemaVersion=2, repository='other/repo', workflow='.github/workflows/release.yml',
                       runId=124, revision='b' * 40, version='0.0.9', target=ci.TARGETS[1],
                       hostSystem='Darwin', hostMachine='arm64', size=129, sha256='b' * 64)
        for key, value in changes.items():
            with self.subTest(key=key):
                self.mutate(**{key: value})
                self.rejected()
                (self.first / 'native.json').write_bytes(original)
        self.mutate(schemaVersion=True)
        self.rejected()

    def test_bad_stamp_schema(self):
        self.mutate(extra='forbidden')
        self.rejected()
        (self.first / 'native.json').write_text('{"runId":123,"runId":123}')
        self.rejected()
        (self.first / 'native.json').write_text('[]')
        self.rejected()

    def test_tampered_or_wrong_isa_binary(self):
        binary = self.first / 'herdr-webapp'
        binary.write_bytes(executable(ci.TARGETS[1]))
        self.mutate(sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
        self.rejected()
        binary.write_bytes(b'not an executable')
        self.rejected()

    def test_empty_oversize_and_total_bound(self):
        binary = self.first / 'herdr-webapp'
        original = binary.read_bytes()
        binary.write_bytes(b'')
        self.rejected()
        binary.write_bytes(original)
        with patch.object(ci, 'MAX_FILE', 127):
            self.rejected()
        with patch.object(ci, 'MAX_TOTAL', 383):
            self.rejected()
        with patch.object(ci, 'MAX_TOTAL', 384):
            self.assemble()

    def test_symlinks_hardlinks_and_special_entries(self):
        for name in ('herdr-webapp', 'native.json'):
            source = self.first / name
            moved = self.root / name
            source.rename(moved)
            source.symlink_to(moved)
            self.rejected()
            source.unlink()
            os.link(moved, source)
            self.rejected()
            source.unlink()
            moved.rename(source)
        moved = self.root / 'native-dir'
        self.first.rename(moved)
        self.first.symlink_to(moved, target_is_directory=True)
        self.rejected()
        self.first.unlink()
        moved.rename(self.first)
        binary = self.first / 'herdr-webapp'
        binary.unlink()
        binary.mkdir()
        self.rejected()

    def test_existing_site_never_overwritten(self):
        self.output.mkdir()
        marker = self.output / 'old'
        marker.write_bytes(b'old site')
        with self.assertRaises(ci.InvalidBuild):
            self.assemble()
        self.assertEqual(marker.read_bytes(), b'old site')

    def test_native_stamp_executes_exact_identity(self):
        target = ci.TARGETS[0]
        output = self.root / 'stamped'
        binary = self.first / 'herdr-webapp'
        with patch.object(ci, 'native') as host, patch.object(ci, 'probe') as probe:
            ci.stamp(binary, output, target, REVISION, VERSION, RUN_ID)
        host.assert_called_once_with(target)
        self.assertEqual(probe.call_args_list, [
            unittest.mock.call(binary, '--package-version', 'herdr-webapp ' + VERSION),
            unittest.mock.call(binary, '--build-revision', REVISION)])
        ci.validate_stamp(ci.load_json(output / 'native.json'), (output / 'herdr-webapp').read_bytes(),
                          target, REVISION, VERSION, RUN_ID)

    def test_stamp_failure_leaves_no_output(self):
        with patch.object(ci, 'native'), patch.object(ci, 'probe', side_effect=ci.InvalidBuild('bad identity')):
            with self.assertRaises(ci.InvalidBuild):
                ci.stamp(self.first / 'herdr-webapp', self.output, ci.TARGETS[0], REVISION, VERSION, RUN_ID)
        self.assertFalse(self.output.exists())

    def test_probe_rejects_version_revision_and_extra_output(self):
        binary = self.root / 'probe'
        binary.write_text('#!/bin/sh\nprintf "herdr-webapp 0.0.8\\n"\n')
        binary.chmod(0o700)
        ci.probe(binary, '--version', 'herdr-webapp ' + VERSION)
        for expected in (REVISION, 'herdr-webapp 0.0.9', 'herdr-webapp 0.0.8\n'):
            with self.assertRaises(ci.InvalidBuild):
                ci.probe(binary, '--version', expected)

    def test_host_rejects_wrong_machine_or_libc(self):
        with patch.object(ci.platform, 'system', return_value='Darwin'), patch.object(ci.platform, 'machine', return_value='x86_64'):
            with self.assertRaises(ci.InvalidBuild):
                ci.native(ci.TARGETS[2])
        with patch.object(ci.platform, 'system', return_value='Linux'), patch.object(ci.platform, 'machine', return_value='aarch64'), patch.object(ci.platform, 'libc_ver', return_value=('musl', '1')):
            with self.assertRaises(ci.InvalidBuild):
                ci.native(ci.TARGETS[1])


class LedgerTests(unittest.TestCase):
    def setUp(self):
        PublisherTests.setUp(self)
        PublisherTests.assemble(self)
        self.publication = (self.output / 'main/publication.json').read_bytes()
        self.manifest = (self.output / 'main/client/manifest.json').read_bytes()
        self.candidate = ci.publication_manifest(self.publication, self.manifest)

    def ledger(self, revisions=None):
        return ci.json_bytes(dict(schemaVersion=1, repository=ci.REPOSITORY,
                                  revisions=revisions or {REVISION: self.candidate[1]}))

    def update(self, ledger=None, publication=None, manifest=None, candidate=None):
        return ci.updated_ledger(candidate or self.candidate, ledger, publication, manifest)

    def test_first_publication_and_guard_fetches(self):
        with patch.object(ci, 'fetch_public', side_effect=[None, None]) as fetch:
            ci.guard(self.output, RUN_ID, 2)
        self.assertEqual(fetch.call_args_list, [
            unittest.mock.call('main/revisions.json', RUN_ID, 2, ci.MAX_LEDGER),
            unittest.mock.call('main/publication.json', RUN_ID, 2, 16384)])
        self.assertEqual((self.output / 'main/revisions.json').read_bytes(), self.ledger())

    def test_legacy_upgrade_and_idempotence(self):
        seeded = self.update(publication=self.publication, manifest=self.manifest)
        self.assertEqual(seeded, self.ledger())
        self.assertEqual(self.update(seeded, self.publication, self.manifest), seeded)
        next_candidate = ('b' * 40, 'c' * 64)
        newer = self.update(seeded, self.publication, self.manifest, next_candidate)
        self.assertEqual(json.loads(newer)['revisions'], {REVISION: self.candidate[1], 'b' * 40: 'c' * 64})

    def test_same_and_historical_revision_changes_rejected(self):
        for revision in (REVISION, 'b' * 40):
            ledger = self.ledger({REVISION: self.candidate[1], 'b' * 40: 'c' * 64})
            with self.assertRaises(ci.InvalidBuild):
                self.update(ledger, self.publication, self.manifest, (revision, 'd' * 64))
        with self.assertRaises(ci.InvalidBuild):
            self.update(None, self.publication, self.manifest, (REVISION, 'd' * 64))

    def test_corrupt_or_missing_history_cannot_reset(self):
        cases = [
            (self.ledger(), None, None),
            (self.ledger({'b' * 40: 'c' * 64}), self.publication, self.manifest),
            (self.ledger({REVISION: 'c' * 64}), self.publication, self.manifest),
            (b'{}', self.publication, self.manifest),
            (b'{"schemaVersion":1,"schemaVersion":1}', self.publication, self.manifest),
            (None, self.publication, None),
            (None, self.publication, self.manifest + b' '),
            (None, b'{}', self.manifest),
        ]
        for ledger, publication, manifest in cases:
            with self.subTest(ledger=ledger), self.assertRaises((ci.InvalidBuild, ValueError)):
                self.update(ledger, publication, manifest)
        with patch.object(ci, 'fetch_public', side_effect=[b'{}', self.publication, self.manifest]):
            with self.assertRaises(ci.InvalidBuild):
                ci.guard(self.output, RUN_ID, 1)
        self.assertFalse((self.output / 'main/revisions.json').exists())

    def test_transport_and_decode_errors_never_write_ledger(self):
        for error in (TimeoutError('timeout'), OSError('TLS failure'), ci.http.client.HTTPException('truncated')):
            with patch.object(ci, 'fetch_public', side_effect=error):
                with self.assertRaises(type(error)):
                    ci.guard(self.output, RUN_ID, 1)
            self.assertFalse((self.output / 'main/revisions.json').exists())
        for data in (b'\xff', b'{', b'NaN', b'[]'):
            with patch.object(ci, 'fetch_public', side_effect=[data, self.publication, self.manifest]):
                with self.assertRaises((ValueError, ci.InvalidBuild)):
                    ci.guard(self.output, RUN_ID, 1)
            self.assertFalse((self.output / 'main/revisions.json').exists())

    def test_strict_legacy_catalog_and_ledger(self):
        for change in ({'repository': 'other/repo'}, {'schemaVersion': True}, {'revision': 'A' * 40}, {'extra': 1}):
            publication = json.loads(self.publication)
            publication.update(change)
            with self.assertRaises(ci.InvalidBuild):
                self.update(None, ci.json_bytes(publication), self.manifest)
        for change in ({'assets': []}, {'version': '1.2.3-rc.1'}, {'protocolVersion': True}, {'revision': 'b' * 40}):
            manifest = json.loads(self.manifest)
            manifest.update(change)
            data = ci.json_bytes(manifest)
            publication = json.loads(self.publication)
            publication['manifestSha256'] = hashlib.sha256(data).hexdigest()
            with self.assertRaises(ci.InvalidBuild):
                self.update(None, ci.json_bytes(publication), data)
        for revisions in ({'A' * 40: 'b' * 64}, {REVISION: 'B' * 64}, {REVISION: True}):
            with self.assertRaises(ci.InvalidBuild):
                self.update(self.ledger(revisions), self.publication, self.manifest)

    def test_ledger_bounds_no_eviction(self):
        with patch.object(ci, 'MAX_LEDGER', len(self.ledger()) - 1):
            with self.assertRaises(ci.InvalidBuild):
                self.update(self.ledger(), self.publication, self.manifest)
            with self.assertRaises(ci.InvalidBuild):
                self.update()
        with patch.object(ci, 'MAX_REVISIONS', 1):
            self.assertEqual(self.update(self.ledger(), self.publication, self.manifest), self.ledger())
            with self.assertRaises(ci.InvalidBuild):
                self.update(self.ledger(), self.publication, self.manifest, ('b' * 40, 'c' * 64))
            with self.assertRaises(ci.InvalidBuild):
                self.update(self.ledger({REVISION: self.candidate[1], 'b' * 40: 'c' * 64}), self.publication, self.manifest)


class HTTPTests(unittest.TestCase):
    def fetch(self, status=200, body=b'{}', headers=None, maximum=10):
        connection = unittest.mock.MagicMock()
        response = connection.getresponse.return_value
        response.status = status
        response.getheader.side_effect = lambda key, default=None: (headers or {}).get(key, default)
        response.read1.side_effect = [body, b'']
        with patch.object(ci.http.client, 'HTTPSConnection', return_value=connection) as constructor:
            result = ci.fetch_public('main/revisions.json', RUN_ID, 2, maximum)
        self.assertEqual(constructor.call_args.args, ('perapp.github.io',))
        self.assertEqual(connection.request.call_args.args, ('GET', '/herdr-webapp/main/revisions.json?run=123&attempt=2'))
        connection.close.assert_called_once()
        return result

    def test_direct_https_status_and_bounds(self):
        self.assertEqual(self.fetch(), b'{}')
        self.assertIsNone(self.fetch(status=404))
        for status in (301, 302, 403, 429, 500):
            with self.assertRaises(ci.InvalidBuild):
                self.fetch(status=status)
        for headers in ({'Content-Length': '11'}, {'Content-Length': '3'}, {'Content-Length': '-1'}, {'Content-Encoding': 'gzip'}):
            with self.assertRaises(ci.InvalidBuild):
                self.fetch(headers=headers)
        with self.assertRaises(ci.InvalidBuild):
            self.fetch(body=b'x' * 11)
        with patch.object(ci.time, 'monotonic', side_effect=[0, 31]):
            with self.assertRaises(ci.InvalidBuild):
                self.fetch()
        with patch.object(ci.http.client, 'HTTPSConnection', side_effect=OSError('TLS failure')):
            with self.assertRaises(OSError):
                ci.fetch_public('main/revisions.json', RUN_ID, 1, 10)


class WorkflowContractTests(unittest.TestCase):
    def test_workflow_contract(self):
        workflow = Path(__file__).resolve().parents[1] / ci.WORKFLOW
        text = workflow.read_text()
        for target in ci.TARGETS:
            self.assertEqual(text.count('target: ' + target), 1)
        for runner in ('ubuntu-22.04', 'ubuntu-24.04-arm', 'macos-15'):
            self.assertIn('runner: ' + runner, text)
        for required in ('make check', 'make release', 'scripts/test_main_ci.py',
                         'scripts/main-ci.py host', 'scripts/main-ci.py stamp', 'scripts/main-ci.py assemble',
                         'HERDR_BUILD_REVISION: ${{ github.sha }}', 'needs: native',
                         'actions/upload-artifact@v4', 'retention-days: 1', 'compression-level: 0', 'overwrite: true',
                         'actions/upload-pages-artifact@v3', 'actions/deploy-pages@v4',
                         'cancel-in-progress: true', 'cancel-in-progress: false',
                         'pages: write', 'id-token: write', 'actions: write',
                         'run_id: context.runId', 'names.has(artifact.name)',
                         'steps.final-head.outputs.current', 'TMPDIR=/private/tmp',
                         'artifact_name: main-pages-${{ github.run_id }}-${{ github.run_attempt }}'):
            self.assertIn(required, text)
        self.assertEqual(text.count('git/ref/heads/main'), 2)
        self.assertLess(text.index('scripts/main-ci.py guard'), text.index('actions/upload-pages-artifact@v3'))
        self.assertIn('--attempt \'${{ github.run_attempt }}\'', text)
        self.assertNotIn('package-release.sh', text)
        self.assertNotIn('contents: write', text)
        self.assertNotIn('secrets.', text)
        self.assertNotIn('gh release', text)
        self.assertNotIn('git tag', text)
        self.assertNotIn('large', '\n'.join(line for line in text.splitlines() if 'runner:' in line))


if __name__ == '__main__':
    unittest.main()
