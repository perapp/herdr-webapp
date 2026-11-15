"""Offline dependency-cache checks; no package manager/network invocation."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('frontend_deps', Path(__file__).with_name('frontend-deps.py'))
deps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deps)


class DependencyTests(unittest.TestCase):
    def test_cache_lock_inputs_missing_tools_force_and_failed_install(self):
        with tempfile.TemporaryDirectory() as directory:
            frontend = Path(directory) / 'frontend'
            frontend.mkdir()
            (frontend / 'package.json').write_text('{}')
            (frontend / 'package-lock.json').write_text('{}')
            calls = []
            fail = False

            def run(argv, **kwargs):
                if argv[-1] == '--version':
                    return subprocess.CompletedProcess(argv, 0, stdout=b'v22.20.0\n')
                self.assertEqual(argv, ['npm', 'ci'])
                self.assertEqual(kwargs['cwd'], frontend)
                calls.append(argv)
                if fail:
                    raise subprocess.CalledProcessError(1, argv)
                binaries = frontend / 'node_modules/.bin'
                binaries.mkdir(parents=True, exist_ok=True)
                for binary in deps.BINS:
                    (binaries / binary).touch()
                return subprocess.CompletedProcess(argv, 0)

            self.assertTrue(deps.prepare(frontend, run=run))
            self.assertFalse(deps.prepare(frontend, run=run))
            (frontend / 'package-lock.json').write_text('{"changed":true}')
            self.assertTrue(deps.prepare(frontend, run=run))
            (frontend / 'node_modules/.bin/vite').unlink()
            self.assertTrue(deps.prepare(frontend, run=run))
            self.assertTrue(deps.prepare(frontend, force=True, run=run))
            fail = True
            with self.assertRaises(subprocess.CalledProcessError):
                deps.prepare(frontend, force=True, run=run)
            self.assertFalse((frontend / 'node_modules/.herdr-deps-sha256').exists())
            self.assertEqual(len(calls), 5)


if __name__ == '__main__':
    unittest.main()
