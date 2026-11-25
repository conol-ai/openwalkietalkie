import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { build, posts, readPost } from '../tools/i18n/build.mjs';
import { parse, walk, attr } from '../tools/i18n/html.mjs';

const PUBLIC = new URL('../public/', import.meta.url).pathname;
const ORIGIN = 'https://openwalkietalkie.org';
const LANGS = [
  { code: 'en', prefix: '', label: 'English' },
  { code: 'zh-Hans', prefix: '/zh-Hans', label: '简体中文' },
  { code: 'zh-Hant', prefix: '/zh-Hant', label: '繁體中文' },
  { code: 'ja', prefix: '/ja', label: '日本語' },
];
const OTHERS = LANGS.slice(1);
const read = file => readFileSync(PUBLIC + file, 'utf8');

function htmlFiles(dir = '') {
  return readdirSync(PUBLIC + dir, { withFileTypes: true }).flatMap(d => {
    const rel = dir + d.name;
    if (d.isDirectory()) return htmlFiles(rel + '/');
    return d.name.endsWith('.html') ? [rel] : [];
  });
}
const all = htmlFiles();
const isTranslated = f => OTHERS.some(l => f.startsWith(l.prefix.slice(1) + '/'));
const english = all.filter(f => !isTranslated(f));
const langOf = f => OTHERS.find(l => f.startsWith(l.prefix.slice(1) + '/')) ?? LANGS[0];
const urlPath = f => '/' + f.replace(/(^|\/)index\.html$/, '$1');
// The file a root-relative URL path is served from, or null.
function served(path) {
  const clean = decodeURIComponent(path.replace(/[?#].*$/, ''));
  const file = PUBLIC + clean.slice(1);
  if (clean.endsWith('/')) return existsSync(file + 'index.html') ? clean.slice(1) + 'index.html' : null;
  if (existsSync(file) && statSync(file).isFile()) return clean.slice(1);
  return existsSync(file + '/index.html') ? clean.slice(1) + '/index.html' : null;
}
const pages = new Map(all.map(f => [f, parse(read(f))]));
const docOf = f => ({ src: read(f), root: pages.get(f) });
const elements = (f, name) => [...walk(pages.get(f))].filter(n => n.type === 'element' && n.name === name);
const text = (src, n) => src.slice(n.innerStart, n.innerEnd).replace(/<[^>]*>/g, '').trim();

test('public/ is exactly what i18n/ builds, so the source and the site cannot drift', () => {
  for (const [out, content] of build()) {
    const file = new URL('../public/' + out, import.meta.url);
    assert.ok(existsSync(file), `${out} is built; run npm run build:i18n`);
    assert.equal(readFileSync(file, 'utf8'), content, `${out} is out of date; run npm run build:i18n`);
  }
});

test('every English page has a zh-Hans, zh-Hant and ja counterpart at the same path', () => {
  assert.ok(english.length >= 29, `found ${english.length} English pages`);
  for (const f of english) for (const l of OTHERS) {
    assert.ok(existsSync(`${PUBLIC}${l.prefix.slice(1)}/${f}`), `${l.code} has ${f}`);
  }
  for (const f of all.filter(isTranslated)) {
    const l = langOf(f);
    assert.ok(english.includes(f.slice(l.prefix.length)), `${f} has an English original`);
  }
});

test('every page declares its language and a complete, reciprocal hreflang set', () => {
  for (const f of all) {
    const lang = langOf(f);
    const html = elements(f, 'html')[0];
    assert.equal(attr(html, 'lang'), lang.code, `${f}: <html lang>`);
    const links = elements(f, 'link').filter(n => attr(n, 'rel') === 'alternate' && attr(n, 'hreflang'));
    const set = Object.fromEntries(links.map(n => [attr(n, 'hreflang'), attr(n, 'href')]));
    assert.deepEqual(Object.keys(set).sort(), ['en', 'ja', 'x-default', 'zh-Hans', 'zh-Hant'], `${f}: hreflang set`);
    assert.equal(links.length, 5, `${f}: one link per language`);
    const base = urlPath(f.slice(lang.prefix ? lang.prefix.length : 0));
    for (const l of LANGS) assert.equal(set[l.code], ORIGIN + l.prefix + base, `${f}: ${l.code} alternate`);
    assert.equal(set['x-default'], ORIGIN + base, `${f}: x-default is English`);
    // Reciprocal: each alternate's page lists the same set.
    for (const l of OTHERS.concat(LANGS[0])) {
      const other = served(l.prefix + base);
      assert.ok(other, `${f}: ${l.code} alternate exists`);
      const back = Object.fromEntries(elements(other, 'link').filter(n => attr(n, 'hreflang')).map(n => [attr(n, 'hreflang'), attr(n, 'href')]));
      assert.deepEqual(back, set, `${other} lists the same alternates as ${f}`);
    }
    const canonical = elements(f, 'link').find(n => attr(n, 'rel') === 'canonical');
    if (canonical) assert.equal(attr(canonical, 'href'), set[lang.code], `${f}: canonical is its own language`);
  }
});

test('every page offers English / 简体中文 / 繁體中文 / 日本語, each landing on the same page', () => {
  for (const f of all) {
    const lang = langOf(f);
    const { root } = docOf(f);
    const box = [...walk(root)].find(n => n.type === 'element' && /\blang-(switch|links)\b/.test(attr(n, 'class') ?? ''));
    assert.ok(box, `${f} has a language switcher`);
    if (attr(box, 'class') === 'lang-switch') {
      assert.ok([...walk(box.parent)].length && box.parent.name === 'nav', `${f}: the switcher is in the header links`);
    }
    const links = [...walk(box)].filter(n => n.type === 'element' && n.name === 'a');
    const src = read(f);
    assert.deepEqual(links.map(n => text(src, n)), LANGS.map(l => l.label), `${f}: switcher labels`);
    assert.deepEqual(links.map(n => attr(n, 'hreflang')), LANGS.map(l => l.code), `${f}: switcher hreflang`);
    assert.deepEqual(links.filter(n => attr(n, 'aria-current')).map(n => attr(n, 'hreflang')), [lang.code], `${f}: current language marked`);
    const base = urlPath(f.slice(lang.prefix ? lang.prefix.length : 0));
    for (const [i, n] of links.entries()) {
      const href = attr(n, 'href');
      assert.ok(served(href), `${f}: switcher link ${href} resolves`);
      const expected = f.endsWith('404.html') ? LANGS[i].prefix + '/' : LANGS[i].prefix + base;
      assert.equal(href, expected, `${f}: ${LANGS[i].code} lands on the same page`);
    }
  }
});

test('every link and asset resolves, and translated pages keep readers in their language', () => {
  const localized = new Set(english.map(urlPath).concat('/blog/feed.xml'));
  const failures = [];
  for (const f of all) {
    const lang = langOf(f);
    for (const n of walk(pages.get(f))) {
      if (n.type !== 'element') continue;
      for (const name of ['href', 'src', 'action']) {
        let value = attr(n, name);
        if (!value || value.startsWith('mailto:') || value.startsWith('data:')) continue;
        if (value.startsWith(ORIGIN + '/')) value = value.slice(ORIGIN.length);
        if (/^[a-z]+:|^\/\//i.test(value)) continue;
        if (value.startsWith('#')) {
          const id = value.slice(1);
          if (id && ![...walk(pages.get(f))].some(m => attr(m, 'id') === id)) failures.push(`${f}: #${id} has no target`);
          continue;
        }
        if (!value.startsWith('/')) { failures.push(`${f}: relative ${name} ${value}`); continue; }
        if (value === '/api/waitlist') continue;
        const target = served(value);
        if (!target) { failures.push(`${f}: ${value} does not exist`); continue; }
        const hash = value.split('#')[1];
        if (hash && pages.has(target) && ![...walk(pages.get(target))].some(m => attr(m, 'id') === hash)) failures.push(`${f}: ${value} has no #${hash}`);
        if (attr(n, 'hreflang')) continue;
        const path = value.replace(/[?#].*$/, '');
        const bare = OTHERS.reduce((p, l) => p.startsWith(l.prefix + '/') ? p.slice(l.prefix.length) : p, path);
        if (localized.has(bare) && path !== lang.prefix + bare) failures.push(`${f}: ${value} leaves ${lang.code}`);
      }
    }
  }
  assert.deepEqual(failures, []);
});

// A few stable sentences per page that must not survive into a translation.
const SENTINELS = {
  'index.html': ['The open-source handheld radio for your AI agents.', 'Fast agents. Slow you.'],
  'manual/index.html': ['Step-by-step OpenWalkieTalkie manual'],
  'docs/index.html': ['Put your agent’s conversations on the deck.'],
  'docs/windows/index.html': ['Install the unsigned Windows preview'],
  'docs/omarchy/index.html': ['Install the OpenWalkieTalkie service and the Omarchy bridge'],
  'docs/native-companions/index.html': ['Build a native companion for any agent.'],
  'privacy/index.html': ['Joining is not an order, reservation, payment'],
  'cookies/index.html': ['The site loads Google Tag Manager after you accept in Cookie settings.'],
  'waitlist/thanks/index.html': ['Thank you for joining the OpenWalkieTalkie hardware waitlist.'],
  '404.html': ['Nobody is on this frequency.'],
  'blog/index.html': ['Every entry as open as the board.', 'READ THE RELEASE'],
};
const CHROME = ['Skip to content', 'Join the waitlist', 'Privacy Policy', 'Cookie information'];

test('translated pages carry no English copy where copy is localised', () => {
  for (const [page, sentences] of Object.entries(SENTINELS)) {
    for (const s of sentences) assert.ok(read(page).includes(s), `sentinel "${s}" is on the English ${page}`);
    for (const l of OTHERS) {
      const html = read(`${l.prefix.slice(1)}/${page}`);
      for (const s of sentences.concat(page === '404.html' ? [] : CHROME)) assert.ok(!html.includes(s), `${l.code}/${page} still says "${s}"`);
    }
  }
  for (const post of posts()) {
    const en = post.by.en;
    const first = /<p>([^<]{40,})/.exec(en.body)?.[1].slice(0, 60);
    for (const l of OTHERS) {
      const html = read(`${l.prefix.slice(1)}/blog/${post.slug}/index.html`);
      for (const s of [en.description, en.heading.replace(/^OpenWalkieTalkie v[\d.]+: /, ''), first].filter(Boolean)) {
        assert.ok(!html.includes(s), `${l.code} ${post.slug} still says "${s}"`);
      }
    }
  }
});

test('translated scripts, privacy notice and the Worker cover every English string', () => {
  const en = JSON.parse(readFileSync(new URL('../i18n/strings/en/js.json', import.meta.url), 'utf8'));
  for (const l of OTHERS) {
    const js = read(`js/i18n/${l.code}.js`);
    const strings = JSON.parse(/window\.OWT_STRINGS = (\{[\s\S]*\});/.exec(js)[1]);
    assert.deepEqual(Object.keys(strings).sort(), Object.values(en).sort(), `${l.code}: every script string`);
    for (const [from, to] of Object.entries(strings)) if (/[a-z]{3,} [a-z]{3,} [a-z]{3,}/.test(from)) assert.notEqual(to, from, `${l.code}: "${from}" is translated`);
    for (const f of all.filter(f => langOf(f) === l)) {
      assert.ok(read(f).includes(`<script src="/js/i18n/${l.code}.js"></script>`), `${f} loads its strings`);
    }
  }
  for (const f of english) assert.ok(!read(f).includes('/js/i18n/'), `${f} loads no translation`);
});

test('a translated post keeps the English markup, images and links', () => {
  for (const post of posts()) {
    assert.deepEqual(Object.keys(post.by).sort(), LANGS.map(l => l.code).sort(), `${post.slug} exists in every language`);
    const imgs = b => [...b.matchAll(/<img\b[^>]*\bsrc="([^"]*)"/g)].map(m => m[1]);
    for (const l of OTHERS) {
      const raw = readPost(new URL(`../i18n/blog/${post.slug}/${l.code}.html`, import.meta.url).pathname);
      assert.deepEqual(imgs(raw.body), imgs(post.by.en.body), `${l.code} ${post.slug}: same images`);
    }
  }
});

test('each language has a blog feed listing its posts; the English feed keeps its shape', () => {
  const entries = xml => [...xml.matchAll(/<entry>[\s\S]*?<\/entry>/g)].map(m => m[0]);
  const en = read('blog/feed.xml');
  assert.match(en, /^<\?xml version="1.0" encoding="utf-8"\?>\n<feed xmlns="http:\/\/www.w3.org\/2005\/Atom">\n  <title>OpenWalkieTalkie Blog<\/title>/);
  const count = entries(en).length;
  assert.equal(count, posts().length);
  for (const l of OTHERS) {
    const xml = read(`${l.prefix.slice(1)}/blog/feed.xml`);
    assert.ok(xml.includes(`<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="${l.code}">`), `${l.code} feed declares its language`);
    assert.ok(xml.includes(`<link rel="self" type="application/atom+xml" href="${ORIGIN}${l.prefix}/blog/feed.xml" />`));
    assert.equal(entries(xml).length, count, `${l.code} feed lists every post`);
    for (const e of entries(xml)) assert.match(e, new RegExp(`<id>${ORIGIN}${l.prefix}/blog/[a-z0-9-]+/</id>`));
    assert.ok(!xml.includes('<title>OpenWalkieTalkie Blog</title>') || l.code === 'en', `${l.code} feed title is translated`);
    for (const f of all.filter(f => langOf(f) === l)) {
      const feedLink = elements(f, 'link').find(n => attr(n, 'type') === 'application/atom+xml');
      if (feedLink) assert.equal(attr(feedLink, 'href'), `${l.prefix}/blog/feed.xml`, `${f} advertises its own feed`);
    }
  }
});

test('the sitemap lists every indexable page in every language with all alternates', () => {
  const xml = read('sitemap.xml');
  const urls = [...xml.matchAll(/<url>([\s\S]*?)<\/url>/g)].map(m => m[1]);
  const locs = urls.map(u => /<loc>([^<]*)<\/loc>/.exec(u)[1]);
  const indexable = english.filter(f => !/<meta name="robots" content="noindex"/.test(read(f)) && f !== 'waitlist/thanks/index.html');
  for (const f of indexable) for (const l of LANGS) assert.ok(locs.includes(ORIGIN + l.prefix + urlPath(f)), `sitemap lists ${l.code} ${f}`);
  assert.equal(new Set(locs).size, locs.length, 'no duplicates');
  for (const u of urls) {
    const alts = [...u.matchAll(/<xhtml:link rel="alternate" hreflang="([^"]+)" href="([^"]+)" \/>/g)].map(m => m[1]);
    assert.deepEqual(alts, ['en', 'zh-Hans', 'zh-Hant', 'ja', 'x-default']);
  }
});
