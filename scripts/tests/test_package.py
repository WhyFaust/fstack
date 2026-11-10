from pathlib import Path
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_package


class PackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / 'package'
        self.folder = self.root / 'skills/example'
        self.folder.mkdir(parents=True)
        self.skill = self.folder / 'SKILL.md'
        self.metadata = {
            'name': 'example', 'description': 'Применяй для примера.', 'license': 'MIT',
            'metadata': {'author': 'WhyFaust', 'source': 'https://github.com/WhyFaust/fstack',
                         'version': '2.0.0'},
        }

    def write_skill(self, metadata):
        self.skill.write_text('---\n' + yaml.safe_dump(metadata, allow_unicode=True)
                              + '---\n# Пример\n', encoding='utf-8')

    def link_directory(self, link, target):
        if sys.platform == 'win32':
            subprocess.run(['cmd', '/c', 'mklink', '/J', str(link), str(target)],
                           check=True, capture_output=True)
            self.addCleanup(link.rmdir)
        else:
            link.symlink_to(target, target_is_directory=True)

    def test_frontmatter_strings_and_name_contract(self):
        self.write_skill(self.metadata)
        self.assertEqual(check_package.skill_metadata(self.skill), self.metadata)
        invalid = (
            {'name': 'example--invalid'}, {'name': 'another'}, {'description': ''},
            {'description': 123}, {'description': 'x' * 1025}, {'license': None},
            {'compatibility': ['Python']}, {'compatibility': 'x' * 501},
            {'allowed-tools': []}, {'metadata': {'author': 'WhyFaust', 'version': 2}},
            {'metadata': {'author': 'another', 'source': 'https://github.com/WhyFaust/fstack'}},
            {'metadata': {**self.metadata['metadata'], 123: 'text'}},
            {'metadata': {**self.metadata['metadata'], True: 'text'}},
        )
        for changes in invalid:
            with self.subTest(changes=changes):
                self.write_skill({**self.metadata, **changes})
                with self.assertRaises(ValueError):
                    check_package.skill_metadata(self.skill)

    def test_package_resources_allow_shared_files_and_reject_missing_or_escape(self):
        shared = self.root / 'LICENSE'
        shared.write_text('License notice', encoding='utf-8')
        self.skill.write_text('[License](../../LICENSE)\n', encoding='utf-8')
        check_package.skill_resources(self.folder, self.root)
        outside = self.root.parent / 'outside.md'
        outside.write_text('External resource', encoding='utf-8')
        for target in ('references/missing.md', '../../../outside.md'):
            with self.subTest(target=target):
                self.skill.write_text(f'[Resource]({target})\n', encoding='utf-8')
                with self.assertRaises(ValueError):
                    check_package.skill_resources(self.folder, self.root)
        self.skill.write_text('```md\n[Example](missing.md)\n```\n', encoding='utf-8')
        check_package.skill_resources(self.folder, self.root)

    def test_optional_resource_directory_requires_content(self):
        directory = self.folder / 'references'
        directory.mkdir()
        placeholder = directory / '.gitkeep'
        placeholder.write_text('', encoding='utf-8')
        with self.assertRaises(ValueError):
            check_package.skill_resources(self.folder, self.root)
        (directory / 'contract.md').write_text('Контракт ресурса', encoding='utf-8')
        check_package.skill_resources(self.folder, self.root)

    def test_nested_resource_link_outside_package_is_rejected_after_valid_file(self):
        outside = self.root.parent / 'external'
        outside.mkdir()
        (outside / 'helper.py').write_text('print(1)\n', encoding='utf-8')
        directory = self.folder / 'scripts'
        directory.mkdir()
        valid = directory / 'local.py'
        valid.write_text('print(2)\n', encoding='utf-8')
        link = directory / 'vendor'
        self.link_directory(link, outside)
        with self.assertRaises(ValueError):
            check_package.skill_resources(self.folder, self.root)

    def test_nested_resource_link_to_shared_package_directory_is_allowed(self):
        shared = self.root / 'shared'
        shared.mkdir()
        (shared / 'helper.py').write_text('print(1)\n', encoding='utf-8')
        directory = self.folder / 'scripts'
        directory.mkdir()
        (directory / 'local.py').write_text('print(2)\n', encoding='utf-8')
        self.link_directory(directory / 'vendor', shared)
        check_package.skill_resources(self.folder, self.root)

    def test_structure_ignores_fences_and_requires_order_and_content(self):
        self.write_skill(self.metadata)
        header = self.skill.read_text(encoding='utf-8')
        sections = ('## Вход и границы\nВход.\n'
                    '## Порядок работы\n```md\n## Чужой заголовок\n```\n'
                    '## Проверка и результат\nРезультат.\n')
        self.skill.write_text(header + sections, encoding='utf-8')
        check_package.native('structure', self.skill)
        invalid = (
            sections.replace('## Вход и границы', '## Иной раздел'),
            sections.replace('Результат.', ''),
            sections + '# Второй заголовок\n',
            sections.replace('## Порядок работы', '## Проверка и результат'),
        )
        for body in invalid:
            with self.subTest(body=body):
                self.skill.write_text(header + body, encoding='utf-8')
                with self.assertRaises(ValueError):
                    check_package.native('structure', self.skill)
        english = self.folder / 'SKILL_en.md'
        english.write_text(header + '## Inputs and scope\nInput.\n'
                           '## Procedure\nDo it.\n## Verification and result\nCheck.\n',
                           encoding='utf-8')
        check_package.native('structure', english)

    def test_percent_links_and_inline_code(self):
        (self.folder / 'space name.md').write_text('Present', encoding='utf-8')
        self.skill.write_text('[Present](space%20name.md#section)\n'
                              '`[Example](missing.md)`\n'
                              '~~~~md\n[Example](missing.md)\n~~~~\n', encoding='utf-8')
        check_package.skill_resources(self.folder, self.root)
        self.skill.write_text('[Missing](space%20missing.md)', encoding='utf-8')
        with self.assertRaises(ValueError):
            check_package.skill_resources(self.folder, self.root)

    def test_complete_package_rejects_broken_release_contracts(self):
        package = self.root / 'complete'
        shutil.copytree(check_package.ROOT, package,
                        ignore=shutil.ignore_patterns('.git', 'target', '__pycache__'))
        check_package.native('check-package', package)
        mutations = (
            ('.codex-plugin/plugin.json', lambda data: {**data, 'version': '01.0.0'}),
            ('.codex-plugin/plugin.json', lambda data: {**data, 'skills': './missing/'}),
            ('.agents/plugins/marketplace.json', lambda data: {**data, 'plugins': []}),
            ('hooks/hooks.json', lambda data: {'hooks': {}}),
        )
        for relative, transform in mutations:
            path = package / relative
            original = path.read_bytes()
            with self.subTest(relative=relative):
                path.write_text(json.dumps(transform(json.loads(original))), encoding='utf-8')
                try:
                    with self.assertRaises(ValueError):
                        check_package.native('check-package', package)
                finally:
                    path.write_bytes(original)
        icon = package / 'assets/fstack.png'
        original = icon.read_bytes()
        icon.write_bytes(b'not a PNG')
        try:
            with self.assertRaises(ValueError):
                check_package.native('check-package', package)
        finally:
            icon.write_bytes(original)

    @unittest.skipUnless(os.name == 'nt', 'Windows glob matching is case-insensitive')
    def test_windows_case_variants_cannot_skip_validation(self):
        package = self.root / 'case-package'
        shutil.copytree(check_package.ROOT, package,
                        ignore=shutil.ignore_patterns('.git', 'target', '__pycache__'))
        skill = package / 'skills/no-comments/SKILL.md'
        original = skill.read_text(encoding='utf-8')
        _, frontmatter, body = original.split('---', 2)
        metadata = yaml.safe_load(frontmatter)
        metadata['license'] = 123
        temporary = skill.with_name('renaming.tmp')
        skill.rename(temporary)
        lower = skill.with_name('skill.md')
        temporary.rename(lower)
        lower.write_text('---\n' + yaml.safe_dump(metadata, allow_unicode=True)
                         + '---' + body, encoding='utf-8')
        with self.assertRaises(ValueError):
            check_package.native('check-package', package)
        lower.write_text(original, encoding='utf-8')
        check_package.native('check-package', package)
        (package / 'UPPER.MD').write_text('[Missing](missing.md)\n', encoding='utf-8')
        with self.assertRaises(ValueError):
            check_package.native('check-package', package)

    def test_shipped_role_defaults_are_valid_toml_and_required(self):
        for path in (check_package.ROOT / 'profile/agents').glob('*.toml'):
            role = tomllib.loads(path.read_text(encoding='utf-8'))
            with self.subTest(role=path.stem):
                check_package.role_settings(role, path.stem)
                check_package.role_settings({**role, 'model': 'custom-provider/model'}, path.stem)
                for field, value in (('model', ''), ('model_reasoning_effort', 'unknown')):
                    with self.assertRaises(ValueError):
                        check_package.role_settings({**role, field: value}, path.stem)


if __name__ == '__main__':
    unittest.main()
