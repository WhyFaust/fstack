from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from check_package import markdown_targets


class MarkdownLinks(unittest.TestCase):
    def test_real_links_remain_visible(self):
        text = '[Справка](docs/guide.md) и ![Маскот](assets/icon.png)'
        self.assertEqual(list(markdown_targets(text)), ['docs/guide.md', 'assets/icon.png'])

    def test_inline_example_is_not_a_package_resource(self):
        text = '`![Пример](./settings-after.png)` и [Справка](guide.md)'
        self.assertEqual(list(markdown_targets(text)), ['guide.md'])

    def test_fenced_examples_are_not_resources(self):
        text = '```md\n[Пример](not-a-file.md)\n```\n~~~text\n![Пример](not-an-image.png)\n~~~\n[Справка](guide.md)'
        self.assertEqual(list(markdown_targets(text)), ['guide.md'])


if __name__ == '__main__':
    unittest.main()
