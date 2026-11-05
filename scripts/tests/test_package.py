from pathlib import Path
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

    def test_frontmatter_strings_and_name_contract(self):
        self.write_skill(self.metadata)
        self.assertEqual(check_package.skill_metadata(self.skill), self.metadata)
        invalid = (
            {'name': 'example--invalid'}, {'name': 'another'}, {'description': ''},
            {'description': 123}, {'description': 'x' * 1025}, {'license': None},
            {'compatibility': ['Python']}, {'compatibility': 'x' * 501},
            {'allowed-tools': []}, {'metadata': {'author': 'WhyFaust', 'version': 2}},
            {'metadata': {'author': 'another', 'source': 'https://github.com/WhyFaust/fstack'}},
        )
        for changes in invalid:
            with self.subTest(changes=changes):
                self.write_skill({**self.metadata, **changes})
                with self.assertRaises(ValueError):
                    check_package.skill_metadata(self.skill)

    def test_package_resources_allow_shared_files_and_reject_missing_or_escape(self):
        shared = self.root / 'NOTICE.md'
        shared.write_text('License notice', encoding='utf-8')
        self.skill.write_text('[Notice](../../NOTICE.md)\n', encoding='utf-8')
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

    def test_shipped_role_defaults_are_valid_toml_and_required(self):
        for path in (check_package.ROOT / 'profile/agents').glob('*.toml'):
            role = tomllib.loads(path.read_text(encoding='utf-8'))
            with self.subTest(role=path.stem):
                check_package.role_settings(role, path.stem)
                for field, value in (('model', 'unknown'), ('model_reasoning_effort', 'unknown')):
                    with self.assertRaises(ValueError):
                        check_package.role_settings({**role, field: value}, path.stem)


if __name__ == '__main__':
    unittest.main()
