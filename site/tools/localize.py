#!/usr/bin/env python3
"""Extract and validate the full-site English translation inventory."""
import argparse
import hashlib
import json
import re
from html import unescape
from html.parser import HTMLParser
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOCALES = {
    'en': ('', 'English'),
    'ja': ('ja/', '日本語'),
    'zh-Hans': ('zh-Hans/', '简体中文'),
    'zh-Hant': ('zh-Hant/', '繁體中文'),
}
PAGES = sorted(
    str(path.relative_to(ROOT / 'public'))
    for path in (ROOT / 'public').rglob('*.html')
    if path.relative_to(ROOT / 'public').parts[0] not in set(LOCALES) - {'en'}
)
BLOCKS = {
    'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'figcaption', 'dt', 'dd',
    'button', 'option', 'summary', 'title', 'text', 'desc',
}
SKIP = {'script', 'style', 'code', 'pre', 'kbd'}
VOID = {
    'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link',
    'meta', 'param', 'source', 'track', 'wbr',
}


class Node:
    def __init__(self, tag, inner, attrs, parent=None):
        self.tag = tag
        self.inner = inner
        self.end = inner
        self.attrs = dict(attrs)
        self.parent = parent


class Parser(HTMLParser):
    def __init__(self, src):
        super().__init__(convert_charrefs=False)
        self.src = src
        self.offsets = [0] + [match.end() for match in re.finditer('\n', src)]
        self.nodes = []
        self.stack = []
        self.text = []

    def pos(self):
        row, column = self.getpos()
        return self.offsets[row - 1] + column

    def handle_starttag(self, tag, attrs):
        node = Node(tag, self.pos() + len(self.get_starttag_text()), attrs,
                    self.stack[-1] if self.stack else None)
        self.nodes.append(node)
        if tag not in VOID:
            self.stack.append(node)

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if tag not in VOID:
            self.stack.pop()

    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, -1, -1):
            if self.stack[index].tag == tag:
                self.stack[index].end = self.pos()
                del self.stack[index:]
                break

    def handle_data(self, data):
        if data.strip():
            self.text.append((self.pos(), self.pos() + len(data),
                              self.stack[-1] if self.stack else None))

    @staticmethod
    def blocked(node):
        while node:
            if node.tag in SKIP or node.attrs.get('translate') == 'no':
                return True
            node = node.parent
        return False

    def segments(self):
        chosen = []
        for node in self.nodes:
            if node.tag not in BLOCKS or self.blocked(node) or node.end <= node.inner:
                continue
            raw = self.src[node.inner:node.end]
            if re.search(r'<(?:input|select|svg|img|canvas)\b', raw):
                continue
            if not any(start <= node.inner and node.end <= end for start, end in chosen):
                chosen.append((node.inner, node.end))
        for start, end, node in self.text:
            if node and not self.blocked(node):
                if not any(left <= start and end <= right for left, right in chosen):
                    chosen.append((start, end))
        return [(start, end, self.src[start:end].strip())
                for start, end in sorted(chosen)
                if re.search('[A-Za-z]', self.src[start:end])]


def key(text):
    return hashlib.sha256(text.encode()).hexdigest()[:12]


def source_text(raw):
    return re.sub(r'\s+', ' ', raw).strip()


def sources():
    result = {}

    def add(text, page):
        text = source_text(text)
        entry = result.setdefault(key(text), {'en': text, 'pages': []})
        if page not in entry['pages']:
            entry['pages'].append(page)

    for page in PAGES:
        parser = Parser((ROOT / 'public' / page).read_text())
        parser.feed(parser.src)
        for _, _, text in parser.segments():
            if re.search('[A-Za-z]', unescape(re.sub('<[^>]+>', '', text))):
                add(text, page)
        for node in parser.nodes:
            if parser.blocked(node):
                continue
            attrs = ['alt', 'aria-label', 'title', 'placeholder']
            if node.tag == 'meta' and (
                node.attrs.get('name') in {'description', 'twitter:title', 'twitter:description'}
                or node.attrs.get('property') in {'og:title', 'og:description', 'og:image:alt'}
            ):
                attrs.append('content')
            for attr in attrs:
                text = node.attrs.get(attr) or ''
                if re.search('[A-Za-z]', text):
                    add(text, page)
    return result


def coverage(source, verbose=False):
    incomplete = False
    for locale in LOCALES:
        if locale == 'en':
            continue
        catalog = json.loads((ROOT / 'locales' / f'{locale}.json').read_text())
        unknown = set(catalog) - set(source)
        if unknown:
            raise SystemExit(f'{locale}: unknown keys {sorted(unknown)}')
        if any(not isinstance(value, str) or not value.strip() for value in catalog.values()):
            raise SystemExit(f'{locale}: empty or invalid translations')
        incomplete = incomplete or bool(set(source) - set(catalog))
        print(f'{locale}: {len(catalog)}/{len(source)} source messages translated')
        if verbose:
            for page in PAGES:
                needed = {k for k, value in source.items() if page in value['pages']}
                done = needed & set(catalog)
                print(f'  {page}: {len(done)}/{len(needed)}')
    return not incomplete


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument('--check', action='store_true', help='Fail if the English inventory is stale.')
    cli.add_argument('--coverage', action='store_true', help='Report coverage by language and page.')
    cli.add_argument('--require-complete', action='store_true', help='Require all target translations.')
    args = cli.parse_args()
    source = sources()
    destination = ROOT / 'locales' / 'en.json'
    if args.check:
        saved = json.loads(destination.read_text()) if destination.exists() else {}
        if saved != source:
            raise SystemExit('English inventory is stale. Run python3 tools/localize.py from site/.')
        print(f'English inventory is current: {len(PAGES)} pages, {len(source)} messages.')
    elif not args.coverage and not args.require_complete:
        destination.write_text(json.dumps(source, ensure_ascii=False, indent=2) + '\n')
        print(f'Extracted {len(source)} messages from {len(PAGES)} pages.')
    if args.coverage or args.require_complete:
        complete = coverage(source, verbose=args.coverage)
        if args.require_complete and not complete:
            raise SystemExit('Translations are incomplete; keep the localization PR in draft.')


if __name__ == '__main__':
    main()
