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
        return {str(p.relative_to(self.home)): p.read_bytes() for p in self.home.rglob('*')
                if p.is_file() and 'backups' not in p.parts}

    def test_dry_run_roundtrip_and_private_backup(self):
        _, _, changes = sync_profile.plan(self.home)
        self.assertEqual(len(changes), 7)
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
        before = self.files()
        with self.assertRaises(sync_profile.ProfileConflict):
            sync_profile.apply_profile(self.home)
        self.assertEqual(self.files(), before)
        result = sync_profile.apply_profile(self.home, adopt=True)
        self.assertEqual((Path(result['backup']) / 'agents/coder.toml').read_bytes(), b'User-owned role')
        role.write_bytes(b'Manual change after adoption')
        before = self.files()
        with self.assertRaises(sync_profile.ProfileConflict):
            sync_profile.apply_profile(self.home, adopt=True)
        self.assertEqual(self.files(), before)

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
