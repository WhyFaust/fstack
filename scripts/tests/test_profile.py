import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sync_profile


class ProfileTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name).resolve() / 'profile'

    def files(self):
        if not self.home.exists():
            return {}
        return {p.relative_to(self.home).as_posix(): p.read_bytes() for p in self.home.rglob('*')
                if p.is_file() and 'backups' not in p.parts}

    def legacy_profile(self):
        hashes = {}
        for name in ('auditor', 'coder', 'python_coder', 'reviewer', 'allure_analyst'):
            relative = f'agents/{name}.toml'
            content = (f'name = "{name}"\ndescription = "Legacy role"\n'
                       'developer_instructions = "Legacy instructions"\n').encode('utf-8')
            path = self.home / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            hashes[relative] = hashlib.sha256(content).hexdigest()
        block = f'{sync_profile.START}\r\nLegacy managed links\r\n{sync_profile.END}'.encode('utf-8')
        (self.home / 'AGENTS.md').write_bytes(b'User instructions\r\n\r\n' + block + b'\r\nUser tail\r\n')
        state = {'schema': 1, 'version': '1.0.0', 'files': hashes,
                 'links_sha256': hashlib.sha256(block).hexdigest()}
        path = self.home / sync_profile.STATE
        path.parent.mkdir(parents=True)
        path.write_bytes((json.dumps(state, indent=2, sort_keys=True) + '\n').encode('utf-8'))
        return self.files()

    def dry_run(self):
        return json.loads(subprocess.check_output(
            [sys.executable, str(Path(sync_profile.__file__).resolve()), '--home', str(self.home)],
            text=True, encoding='utf-8',
        ))

    def test_dry_run_roundtrip_and_private_backup(self):
        _, _, changes = sync_profile.plan(self.home)
        self.assertEqual(set(changes), {
            'agents/auditor.toml', 'agents/coder.toml', 'agents/reviewer.toml',
            'agents/verifier.toml', 'agents/allure_analyst.toml', 'AGENTS.md', 'fstack/profile.json',
        })
        self.assertFalse(self.home.exists())
        self.home.mkdir()
        original = self.home / 'AGENTS.md'
        original.write_bytes(b'User instructions\r\n')
        original.chmod(0o600)
        previous = os.umask(0o022)
        try:
            result = sync_profile.apply_profile(self.home)
        finally:
            os.umask(previous)
        self.assertEqual(result['removed'], [])
        self.assertEqual(result['unmanaged_retired'], [])
        backup = Path(result['backup'])
        self.assertEqual((backup / 'AGENTS.md').read_bytes(), b'User instructions\r\n')
        self.assertNotIn(b'\n', original.read_bytes().replace(b'\r\n', b''))
        if os.name == 'nt':
            environment = {**os.environ, 'FSTACK_TEST_BACKUP': str(backup)}
            command = "$ErrorActionPreference = 'Stop'; $acl = Get-Acl -LiteralPath $env:FSTACK_TEST_BACKUP; @{Protected=$acl.AreAccessRulesProtected; Owner=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; Rules=@($acl.Access | ForEach-Object { $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value })} | ConvertTo-Json -Compress"
            observed = json.loads(subprocess.check_output(
                [shutil.which('pwsh') or 'powershell.exe', '-NoProfile', '-NonInteractive', '-Command', command],
                env=environment, text=True,
            ))
            self.assertTrue(observed['Protected'])
            self.assertEqual(set(observed['Rules']), {observed['Owner'], 'S-1-5-18'})
        else:
            self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o700)
            for path in backup.rglob('*'):
                if path.is_file():
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        before = self.files()
        backups = list(backup.parent.iterdir())
        self.assertEqual(sync_profile.apply_profile(self.home)['status'], 'unchanged')
        self.assertEqual(self.files(), before)
        self.assertEqual(list(backup.parent.iterdir()), backups)

    def test_adoption_and_managed_conflicts_preserve_user_content(self):
        role = self.home / 'agents/coder.toml'
        role.parent.mkdir(parents=True)
        role.write_bytes(b'User-owned role')
        retired = self.home / 'agents/python_coder.toml'
        retired.write_bytes(b'User-owned Python role')
        before = self.files()
        with self.assertRaises(sync_profile.ProfileConflict):
            sync_profile.apply_profile(self.home)
        self.assertEqual(self.files(), before)
        result = sync_profile.apply_profile(self.home, adopt=True)
        self.assertEqual((Path(result['backup']) / 'agents/coder.toml').read_bytes(), b'User-owned role')
        self.assertEqual(result['removed'], [])
        self.assertEqual(result['unmanaged_retired'], ['agents/python_coder.toml'])
        self.assertEqual(retired.read_bytes(), b'User-owned Python role')
        self.assertFalse((Path(result['backup']) / 'agents/python_coder.toml').exists())
        repeat = self.dry_run()
        self.assertEqual(repeat['status'], 'unchanged')
        self.assertEqual(repeat['removed'], [])
        self.assertEqual(repeat['unmanaged_retired'], ['agents/python_coder.toml'])
        role.write_bytes(b'Manual change after adoption')
        before = self.files()
        for adopt in (False, True):
            with self.assertRaises(sync_profile.ProfileConflict):
                sync_profile.apply_profile(self.home, adopt=adopt)
            self.assertEqual(self.files(), before)

    def test_legacy_migration_retires_owned_role_and_uses_canonical_instructions(self):
        before = self.legacy_profile()
        retired = 'agents/python_coder.toml'
        planned = self.dry_run()
        self.assertEqual(planned['status'], 'planned')
        self.assertEqual(planned['removed'], [retired])
        self.assertEqual(planned['unmanaged_retired'], [])
        self.assertIn(retired, planned['changes'])
        self.assertIn('agents/verifier.toml', planned['changes'])
        self.assertEqual(self.files(), before)
        self.assertFalse((self.home / 'fstack/backups').exists())
        _, _, changes = sync_profile.plan(self.home)
        self.assertIsNone(changes[retired])
        result = sync_profile.apply_profile(self.home)
        self.assertEqual(result['removed'], [retired])
        self.assertEqual(result['unmanaged_retired'], [])
        self.assertEqual(result['changes'], planned['changes'])
        self.assertFalse((self.home / retired).exists())
        expected_roles = {'auditor', 'coder', 'reviewer', 'verifier', 'allure_analyst'}
        self.assertEqual({path.stem for path in (self.home / 'agents').glob('*.toml')}, expected_roles)
        backup = Path(result['backup'])
        self.assertEqual((backup / retired).read_bytes(), before[retired])
        self.assertEqual((backup / sync_profile.STATE).read_bytes(), before[sync_profile.STATE])
        receipt = json.loads((backup / 'manifest.json').read_text(encoding='utf-8'))
        self.assertEqual(receipt['removed'], [retired])
        self.assertEqual(receipt['created'], ['agents/verifier.toml'])
        instructions = (sync_profile.ROOT / 'profile/AGENTS.md').read_text(encoding='utf-8').strip()
        block = (sync_profile.START + '\r\n' + instructions.replace('\n', '\r\n')
                 + '\r\n' + sync_profile.END).encode('utf-8')
        self.assertEqual((self.home / 'AGENTS.md').read_bytes(),
                         b'User instructions\r\n\r\n' + block + b'\r\nUser tail\r\n')
        state = json.loads((self.home / sync_profile.STATE).read_text(encoding='utf-8'))
        self.assertEqual(state['schema'], 1)
        self.assertEqual(set(state['files']), {f'agents/{name}.toml' for name in expected_roles})
        self.assertEqual(state['links_sha256'], hashlib.sha256(block).hexdigest())
        after = self.files()
        backups = list(backup.parent.iterdir())
        repeat = sync_profile.apply_profile(self.home)
        self.assertEqual(repeat['status'], 'unchanged')
        self.assertEqual(repeat['changes'], [])
        self.assertEqual(repeat['removed'], [])
        self.assertEqual(repeat['unmanaged_retired'], [])
        self.assertEqual(self.files(), after)
        self.assertEqual(list(backup.parent.iterdir()), backups)

    def test_legacy_conflicts_and_invalid_state_prevent_all_writes(self):
        directory = self.home.parent
        for relative in ('agents/coder.toml', 'agents/python_coder.toml', 'AGENTS.md'):
            for missing in (False, True):
                with self.subTest(path=relative, missing=missing):
                    self.home = directory / f'{relative.replace("/", "_")}-{missing}'
                    self.legacy_profile()
                    path = self.home / relative
                    if missing:
                        path.unlink()
                    elif relative == 'AGENTS.md':
                        path.write_bytes(path.read_bytes().replace(b'Legacy managed links', b'User instructions'))
                    else:
                        path.write_bytes(b'User-edited role')
                    before = self.files()
                    for adopt in (False, True):
                        with self.assertRaises(sync_profile.ProfileConflict) as observed:
                            sync_profile.apply_profile(self.home, adopt=adopt)
                        expected = 'AGENTS.md:managed-block' if relative == 'AGENTS.md' else relative
                        self.assertEqual(observed.exception.paths, [expected])
                        self.assertEqual(self.files(), before)
                        self.assertFalse((self.home / 'fstack/backups').exists())
        self.home = directory / 'invalid-state'
        self.legacy_profile()
        state_path = self.home / sync_profile.STATE
        state = json.loads(state_path.read_text(encoding='utf-8'))
        unknown_paths = dict(state['files'])
        unknown_paths['../outside.toml'] = unknown_paths.pop('agents/python_coder.toml')
        invalid_states = (
            None,
            {**state, 'schema': True},
            {**state, 'files': []},
            {**state, 'files': unknown_paths},
            {**state, 'files': {**state['files'], 'agents/python_coder.toml': 7}},
            {**state, 'files': {**state['files'], 'agents/python_coder.toml': 'z' * 64}},
            {**state, 'links_sha256': []},
            {**state, 'links_sha256': '0' * 63},
        )
        for index, invalid in enumerate(invalid_states):
            with self.subTest(invalid_state=index):
                state_path.write_bytes(json.dumps(invalid).encode('utf-8'))
                before = self.files()
                for adopt in (False, True):
                    with self.assertRaises(ValueError) as observed:
                        sync_profile.apply_profile(self.home, adopt=adopt)
                    self.assertNotIsInstance(observed.exception, sync_profile.ProfileConflict)
                    self.assertEqual(self.files(), before)
                    self.assertFalse((self.home / 'fstack/backups').exists())

    def test_retirement_failure_restores_profile_but_preserves_concurrent_recreation(self):
        directory = self.home.parent
        real_write = sync_profile.atomic_write
        for concurrent in (False, True):
            with self.subTest(concurrent=concurrent):
                self.home = directory / str(concurrent)
                before = self.legacy_profile()
                retired = self.home / 'agents/python_coder.toml'
                previous_role = retired.read_bytes()

                def write(path, content):
                    if path == self.home / sync_profile.STATE:
                        self.assertFalse(retired.exists())
                        if concurrent:
                            retired.write_bytes(b'Concurrent user recreation')
                        raise OSError('Injected state write failure')
                    real_write(path, content)

                with patch.object(sync_profile, 'atomic_write', side_effect=write):
                    with self.assertRaises(ExceptionGroup if concurrent else OSError) as observed:
                        sync_profile.apply_profile(self.home)
                if concurrent:
                    before['agents/python_coder.toml'] = b'Concurrent user recreation'
                    self.assertTrue(any(isinstance(e, OSError) for e in observed.exception.exceptions))
                    self.assertTrue(any(isinstance(e, sync_profile.ProfileConflict) for e in observed.exception.exceptions))
                self.assertEqual(self.files(), before)
                self.assertFalse((self.home / 'fstack/profile.lock').exists())
                backup = next((self.home / 'fstack/backups').iterdir())
                receipt = json.loads((backup / 'manifest.json').read_text(encoding='utf-8'))
                self.assertEqual(receipt['removed'], ['agents/python_coder.toml'])
                self.assertEqual((backup / 'agents/python_coder.toml').read_bytes(), previous_role)
                self.assertEqual(hashlib.sha256(previous_role).hexdigest(),
                                 receipt['files']['agents/python_coder.toml'])

    def test_write_failure_rolls_back_owned_writes_but_preserves_concurrent_edit(self):
        real_write = sync_profile.atomic_write
        for concurrent in (False, True):
            with self.subTest(concurrent=concurrent):
                self.home = self.home.parent / str(concurrent)
                self.home.mkdir()
                (self.home / 'AGENTS.md').write_bytes(b'User text')
                before = self.files()

                def write(path, content):
                    if path == self.home / 'agents/coder.toml':
                        if concurrent:
                            (self.home / 'agents/auditor.toml').write_bytes(b'Concurrent user edit')
                        raise OSError('Injected write failure')
                    real_write(path, content)

                with patch.object(sync_profile, 'atomic_write', side_effect=write):
                    with self.assertRaises(ExceptionGroup if concurrent else OSError) as observed:
                        sync_profile.apply_profile(self.home)
                if concurrent:
                    self.assertEqual((self.home / 'agents/auditor.toml').read_bytes(), b'Concurrent user edit')
                    self.assertTrue(any(isinstance(e, OSError) for e in observed.exception.exceptions))
                    self.assertTrue(any(isinstance(e, sync_profile.ProfileConflict) for e in observed.exception.exceptions))
                else:
                    self.assertEqual(self.files(), before)
                self.assertEqual((self.home / 'AGENTS.md').read_bytes(), b'User text')
                self.assertFalse((self.home / 'fstack/profile.lock').exists())

    def test_backup_protection_failure_prevents_profile_writes(self):
        self.home.mkdir()
        (self.home / 'AGENTS.md').write_bytes(b'Private original')
        before = self.files()
        with patch.object(sync_profile, 'private_backup', side_effect=OSError('Cannot protect backup')):
            with self.assertRaises(OSError):
                sync_profile.apply_profile(self.home)
        self.assertEqual(self.files(), before)


if __name__ == '__main__':
    unittest.main()
