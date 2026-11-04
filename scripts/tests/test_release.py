import importlib.util
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release


def git(directory, *args):
    return subprocess.check_output(['git', '-c', 'commit.gpgsign=false', *args], cwd=directory,
                                   text=True, encoding='utf-8', stderr=subprocess.PIPE).strip()


class ReleaseTests(unittest.TestCase):
    def fixture(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name) / 'repo'
        (root / 'scripts').mkdir(parents=True)
        shutil.copyfile(Path(release.__file__), root / 'scripts/release.py')
        (root / '.codex-plugin').mkdir()
        (root / '.codex-plugin/plugin.json').write_text(json.dumps({'name': 'fstack', 'version': '0.2.0'}))
        (root / '.gitignore').write_text('__pycache__/\ndist/\n')
        for name in ('alpha', 'beta'):
            folder = root / 'skills' / name
            folder.mkdir(parents=True)
            (folder / 'SKILL.md').write_text(f'---\nname: {name}\nmetadata:\n  version: 0.2.0\n---\n# Original body {name}\n')
        git(root, 'init', '--initial-branch=main')
        git(root, 'config', 'user.name', 'Fixture')
        git(root, 'config', 'user.email', 'fixture@example.invalid')
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'chore: baseline')
        git(root, 'tag', 'v0.2.0')
        spec = importlib.util.spec_from_file_location('fixture_release', root / 'scripts/release.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.release_records = lambda repository: [{'tag_name': 'v0.2.0', 'draft': False, 'prerelease': False}]
        return root, module

    def test_stable_semver_and_conventional_commit_contract(self):
        for invalid in ('01.2.0', '1.02.0', '1.2.03', '1.2', '1.2.3-alpha', None, 1):
            with self.subTest(version=invalid), self.assertRaises(ValueError):
                release.version_tuple(invalid)
        self.assertEqual([release.increment('1.2.3', level) for level in (0, 1, 2, 3)],
                         ['1.2.3', '1.2.4', '1.3.0', '2.0.0'])
        for messages, expected in ((['docs: text'], 0), (['fix(profile): text'], 1),
                                   (['perf: text'], 1), (['fix: text', 'feat: text'], 2),
                                   (['refactor!: text'], 3), (['chore: text\n\nBREAKING CHANGE: contract'], 3)):
            with self.subTest(messages=messages):
                self.assertEqual(release.change_level(messages), expected)

    def test_pending_release_preserves_bodies_and_does_not_double_bump(self):
        root, module = self.fixture()
        interface = root / 'skills/alpha/agents/openai.yaml'
        interface.parent.mkdir()
        interface.write_text('interface:\n  display_name: New behavior\n')
        untouched = root / 'skills/beta/SKILL.md'
        original = untouched.read_bytes()
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'feat: new behavior')
        proposed = module.plan('fixture/fstack')
        self.assertEqual(proposed['version'], '0.3.0')
        self.assertEqual(proposed['skills'], ['alpha'])
        changed = module.set_version('0.3.0', proposed['skills'])
        self.assertEqual({path.relative_to(module.ROOT).as_posix() for path in changed},
                         {'.codex-plugin/plugin.json', 'skills/alpha/SKILL.md'})
        self.assertEqual(module.check_versions(), '0.3.0')
        self.assertEqual(untouched.read_bytes(), original)
        self.assertEqual(module.set_version('0.3.0', proposed['skills']), [])
        for name in ('alpha', 'beta'):
            self.assertTrue((root / 'skills' / name / 'SKILL.md').read_text().endswith(f'# Original body {name}\n'))
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'chore(release): v0.3.0')
        (root / 'readme.md').write_text('Documentation')
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'docs: clarify')
        repeated = module.plan('fixture/fstack')
        self.assertEqual(repeated['version'], '0.3.0')
        self.assertEqual(repeated['skills'], ['alpha'])

    def test_skill_changes_use_final_tree_and_ignore_version_only_edits(self):
        root, module = self.fixture()
        skill = root / 'skills/alpha/SKILL.md'
        text = skill.read_text().replace('version: 0.2.0', 'version: "0.2.0" # keep comment')
        skill.write_text(text)
        module.set_version('0.2.1', ['alpha'])
        self.assertEqual(skill.read_text(), text.replace('"0.2.0"', '0.2.1'))
        self.assertEqual(module.check_versions(), '0.2.1')
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'chore: version-only fixture')
        self.assertEqual(module.changed_skills('v0.2.0'), [])
        resources = {'skills/alpha/references/run.md': b'New reference',
                     'skills/beta/assets/icon.bin': b'\x00\x01\xff',
                     'skills/gamma/SKILL.md': b'---\nname: gamma\nmetadata:\n  version: 0.2.1\n---\nNew skill\n'}
        for name, content in resources.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'feat: references, asset, and new skill')
        self.assertEqual(module.changed_skills('v0.2.0'), ['alpha', 'beta', 'gamma'])
        (root / 'skills/alpha/references/run.md').unlink()
        git(root, 'add', '-u')
        git(root, 'commit', '-m', 'refactor: undo reference addition')
        self.assertEqual(module.changed_skills('v0.2.0'), ['beta', 'gamma'])
        skill.write_text(skill.read_text().replace('version: 0.2.1', 'version: 9.0.0'))
        with self.assertRaisesRegex(ValueError, 'новее плагина'):
            module.check_versions()

    def test_event_and_live_main_must_match_before_publication(self):
        root, module = self.fixture()
        base = git(root, 'rev-parse', 'HEAD')
        origin = root.parent / 'origin.git'
        git(root.parent, 'clone', '--bare', str(root), str(origin))
        git(root, 'remote', 'add', 'origin', str(origin))
        writer = root.parent / 'writer'
        git(root.parent, 'clone', str(origin), str(writer))
        git(writer, 'config', 'user.name', 'Fixture')
        git(writer, 'config', 'user.email', 'fixture@example.invalid')
        (writer / 'change.txt').write_text('New unvalidated push')
        git(writer, 'add', '.')
        git(writer, 'commit', '-m', 'fix: new push')
        newer = git(writer, 'rev-parse', 'HEAD')
        git(writer, 'push', 'origin', 'main')
        real_run = module.run

        def local_only(*args):
            self.assertNotEqual(args[0], 'gh', 'A rejected release must not call GitHub')
            return real_run(*args)

        for expected, reason in ((newer, 'проверенного SHA'), (base, 'main изменился')):
            environment = {'GITHUB_ACTIONS': 'true', 'GITHUB_REF': 'refs/heads/main',
                           'GITHUB_REPOSITORY': 'fixture/fstack', 'GITHUB_SHA': expected}
            with self.subTest(expected=expected), patch.dict(os.environ, environment), patch.object(module, 'run', side_effect=local_only):
                with self.assertRaisesRegex(ValueError, reason):
                    module.publish('fixture/fstack')
        self.assertEqual(git(root, 'rev-parse', 'HEAD'), base)
        self.assertEqual(module.check_versions(), '0.2.0')
        self.assertFalse((root / 'dist').exists())

    def test_draft_ownership_and_tag_identity_guard_real_archive(self):
        root, module = self.fixture()
        module.set_version('0.2.1', [])
        git(root, 'add', '.')
        git(root, 'commit', '-m', 'chore(release): v0.2.1')
        sha = git(root, 'rev-parse', 'HEAD')
        record = {'tag_name': 'v0.2.1', 'draft': True, 'body': f'<!-- release-source:{sha} -->',
                  'author': {'login': 'human'}}
        real_run = module.run
        calls = []

        def intercepted(*args):
            if args[0] == 'gh':
                calls.append(args)
                return ''
            return real_run(*args)

        with patch.object(module, 'run', side_effect=intercepted):
            with self.assertRaisesRegex(ValueError, 'не принадлежит'):
                module.finish_release('fixture/fstack', '0.2.1', [record], sha, sha)
            self.assertEqual(calls, [])
            record['author']['login'] = 'github-actions[bot]'
            with redirect_stdout(io.StringIO()):
                module.finish_release('fixture/fstack', '0.2.1', [record], sha, sha)
            self.assertEqual([call[2] for call in calls], ['upload', 'edit'])
            artifact = root / 'dist/fstack-0.2.1.zip'
            with zipfile.ZipFile(artifact) as archive:
                self.assertEqual(json.loads(archive.read('fstack/.codex-plugin/plugin.json'))['version'], '0.2.1')
            self.assertEqual(artifact.with_suffix('.zip.sha256').read_text().split()[0], hashlib.sha256(artifact.read_bytes()).hexdigest())
            calls.clear()
            (root / 'other.txt').write_text('Other commit')
            git(root, 'add', 'other.txt')
            git(root, 'commit', '-m', 'chore: other commit')
            git(root, 'tag', 'v0.2.1')
            with self.assertRaisesRegex(ValueError, 'другую ревизию'):
                module.finish_release('fixture/fstack', '0.2.1', [], sha, sha)
            self.assertEqual(calls, [])

    def test_rerun_recovers_only_exact_version_commit_without_double_publication(self):
        for tampered in (False, True):
            with self.subTest(tampered=tampered):
                root, module = self.fixture()
                skill = root / 'skills/alpha/SKILL.md'
                skill.write_text(skill.read_text() + 'Validated instruction\n')
                git(root, 'add', '.')
                git(root, 'commit', '-m', 'fix: validated change')
                event = git(root, 'rev-parse', 'HEAD')
                self.assertEqual(module.changed_skills('v0.2.0'), ['alpha'])
                module.set_version('0.2.1', ['alpha'])
                if tampered:
                    skill = root / 'skills/alpha/SKILL.md'
                    skill.write_text(skill.read_text() + 'Unvalidated instruction\n')
                git(root, 'add', '.')
                git(root, 'commit', '-m', 'chore(release): v0.2.1')
                candidate = git(root, 'rev-parse', 'HEAD')
                origin = root.parent / 'origin.git'
                git(root.parent, 'clone', '--bare', str(root), str(origin))
                git(root, 'remote', 'add', 'origin', str(origin))
                git(root, 'checkout', '--detach', event)
                records = [*module.release_records('fixture/fstack'),
                           {'tag_name': 'v0.2.1', 'draft': True, 'prerelease': False,
                            'author': {'login': 'github-actions[bot]'},
                            'body': f'<!-- release-source:{candidate} -->'}]
                module.release_records = lambda repository: records
                calls, real_run = [], module.run

                def intercepted(*args):
                    if args[0] != 'gh':
                        return real_run(*args)
                    calls.append(args)
                    if args[1:3] == ('release', 'edit'):
                        git(root, 'tag', 'v0.2.1', candidate)
                        git(root, 'push', 'origin', 'v0.2.1')
                        records[-1]['draft'] = False
                    return ''

                environment = {'GITHUB_ACTIONS': 'true', 'GITHUB_REF': 'refs/heads/main',
                               'GITHUB_REPOSITORY': 'fixture/fstack', 'GITHUB_SHA': event}
                with patch.dict(os.environ, environment), patch.object(module, 'run', side_effect=intercepted), redirect_stdout(io.StringIO()):
                    if tampered:
                        with self.assertRaisesRegex(ValueError, 'main изменился'):
                            module.publish('fixture/fstack')
                        self.assertEqual(git(root, 'rev-parse', 'HEAD'), event)
                        self.assertEqual(calls, [])
                        self.assertFalse((root / 'dist').exists())
                    else:
                        module.publish('fixture/fstack')
                        self.assertEqual([call[2] for call in calls], ['upload', 'edit'])
                        self.assertEqual(git(root, 'rev-parse', 'HEAD'), candidate)
                        self.assertEqual(module.check_versions(), '0.2.1')
                        self.assertIn('version: 0.2.0', (root / 'skills/beta/SKILL.md').read_text())
                        self.assertTrue((root / 'dist/fstack-0.2.1.zip').is_file())
                        calls.clear()
                        git(root, 'checkout', '--detach', event)
                        module.publish('fixture/fstack')
                        self.assertEqual(calls, [])
                        self.assertEqual(git(root, 'rev-parse', 'HEAD'), candidate)


if __name__ == '__main__':
    unittest.main()
