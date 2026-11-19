"""Inventory tests: prevent skipped user copy and accidental code translation."""
import importlib.util
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('localize', Path(__file__).parents[1] / 'tools/localize.py')
localize = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(localize)


def extract(html):
    parser = localize.Parser(html)
    parser.feed(html)
    return [localize.source_text(text) for _, _, text in parser.segments()
            if localize.re.search('[A-Za-z]', localize.unescape(localize.re.sub('<[^>]+>', '', text)))]


class LocalizationInventoryTests(unittest.TestCase):
    def test_inline_markup_and_entities_stay_in_one_message(self):
        self.assertEqual(extract('<p>Read <a href="/docs/">the docs</a> &amp; use <b>PTT</b>.</p>'),
                         ['Read <a href="/docs/">the docs</a> &amp; use <b>PTT</b>.'])

    def test_code_and_nontranslatable_subtrees_are_excluded(self):
        html = '<script>const name = "Hidden";</script><style>.hidden{}</style><pre>codex --help</pre><kbd>ENTER</kbd><div translate="no"><p>Product token</p></div><p>Visible prose.</p>'
        self.assertEqual(extract(html), ['Visible prose.'])

    def test_nested_blocks_do_not_duplicate_messages(self):
        self.assertEqual(extract('<figcaption><h3>Title</h3><p>Caption</p></figcaption>'),
                         ['<h3>Title</h3><p>Caption</p>'])

    def test_offsets_survive_unicode_and_self_closing_tags(self):
        self.assertEqual(extract('<div>日本語</div><img src="a.png"/><p>Hello<br/>world.</p>'),
                         ['Hello<br/>world.'])

    def test_no_translatable_text_in_empty_icon_markup(self):
        self.assertEqual(extract('<button><span class="icon"></span></button>'), [])

    def test_every_public_english_page_is_in_scope(self):
        for page in ['index.html', 'manual/index.html', 'privacy/index.html', 'cookies/index.html',
                     'docs/omarchy/index.html', 'docs/windows/index.html', 'docs/native-companions/index.html',
                     'blog/index.html', '404.html', 'waitlist/thanks/index.html']:
            self.assertIn(page, localize.PAGES)
        self.assertGreaterEqual(len(localize.PAGES), 28)

    def test_inventory_is_current_and_metadata_is_present(self):
        source = localize.sources()
        committed = json.loads((localize.ROOT / 'locales/en.json').read_text())
        self.assertEqual(source, committed)
        values = {v['en'] for v in source.values()}
        self.assertIn('Toggle day and night theme', values)
        self.assertTrue(any('OpenWalkieTalkie is an open handheld' in text for text in values))

    def test_starter_catalogs_have_no_unknown_or_empty_entries(self):
        source = localize.sources()
        for locale in ['ja', 'zh-Hans', 'zh-Hant']:
            catalog = json.loads((localize.ROOT / f'locales/{locale}.json').read_text())
            self.assertTrue(set(catalog) <= set(source))
            self.assertTrue(all(isinstance(v, str) and v.strip() for v in catalog.values()))
            self.assertGreaterEqual(len(catalog), 60)
            self.assertLess(len(catalog), len(source), 'Starter catalogs must not claim complete coverage')


if __name__ == '__main__':
    unittest.main()
