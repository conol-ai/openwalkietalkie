#!/usr/bin/env node
// Build every page of the site in every language from one source.
//
//   npm run build:i18n            write public/ (pages, feeds, sitemap, JS strings)
//   npm run build:i18n -- --check fail if public/ is not what the source builds
//
// Source, all under i18n/:
//   site.json                       languages and product pages
//   templates/<id>.html             page structure, copy replaced by {{key}}
//   strings/<lang>/<id>.json        copy for one page; common.json for shared copy
//   strings/<lang>/js.json          copy that the site's scripts show
//   blog/<slug>/<lang>.html         one post: front matter, then its article markup
//
// English is written at the root exactly as before; other languages under
// their prefix (/zh-Hans/…). Running it twice changes no bytes.
import { readFileSync, writeFileSync, existsSync, readdirSync, mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { createHash } from 'node:crypto';
import { SITE, readJson, render } from './lib.mjs';
import { parse, walk } from './html.mjs';

const I18N = `${SITE}/i18n`;
const PUBLIC = `${SITE}/public`;
const site = readJson(`${I18N}/site.json`);
const documents = readJson(`${I18N}/documents/manifest.json`);
const { origin, languages } = site;
const EN = languages[0];
const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

/** The URL path of an output file: index.html → /, docs/index.html → /docs/. */
export const pathOf = out => '/' + out.replace(/(^|\/)index\.html$/, '$1');
const outOf = (lang, out) => (lang.prefix ? lang.prefix.slice(1) + '/' : '') + out;

/** A display date for an ISO day (2026-10-15). */
export function formatDate(lang, iso) {
  const [y, m, d] = iso.split('-').map(Number);
  return lang.code === 'en' ? `${MONTHS[m - 1]} ${d}, ${y}` : `${y}年${m}月${d}日`;
}

const strings = {};
function table(lang, id) {
  const file = `${I18N}/strings/${lang.code}/${id}.json`;
  if (!existsSync(file)) throw new Error(`missing ${file.slice(SITE.length + 1)}`);
  return readJson(file);
}
function stringsFor(lang, id) {
  const key = `${lang.code}/${id}`;
  if (!strings[key]) {
    const merged = { ...table(lang, 'common'), ...(id === 'common' ? {} : table(lang, id)) };
    if (lang !== EN) checkTable(lang, id, merged);
    strings[key] = merged;
  }
  return strings[key];
}

// Markup a translation must keep: every tag, with its href, src and other
// attributes. Blocks (paragraphs, headings, list items, figures) stay in
// order; inline elements (links, code, emphasis) must all be there but may
// move within their sentence, as word order differs between languages.
// Visible copy and the text attributes around it are what may change.
const TEXT_ATTRS = new Set(['alt', 'title', 'aria-label', 'placeholder', 'content']);
const INLINE = new Set(['a', 'abbr', 'b', 'br', 'cite', 'code', 'em', 'i', 'img', 'kbd', 'mark', 'q', 's', 'samp', 'small', 'span', 'strong', 'sub', 'sup', 'time', 'u', 'var', 'wbr']);
export function skeleton(html) {
  const tag = n => n.name + n.attrs.filter(a => !TEXT_ATTRS.has(a.name)).map(a => ` ${a.name}=${a.value ?? ''}`).join('');
  const out = [];
  const visit = (node, bag) => {
    for (const n of node.children ?? []) {
      if (n.type !== 'element') continue;
      if (INLINE.has(n.name)) { bag.push(tag(n)); visit(n, bag); continue; }
      out.push(tag(n));
      const inner = [];
      visit(n, inner);
      out.push(`[${inner.sort().join(',')}]`);
    }
  };
  const top = [];
  visit(parse(html), top);
  return out.join('|') + `|[${top.sort().join(',')}]`;
}
function checkTable(lang, id, merged) {
  const en = { ...table(EN, 'common'), ...(id === 'common' ? {} : table(EN, id)) };
  for (const key of Object.keys(en)) {
    if (!(key in merged)) throw new Error(`${lang.code}/${id}: missing ${key}`);
    if (skeleton(en[key]) !== skeleton(merged[key])) throw new Error(`${lang.code}/${id}: ${key} changes the markup\n  en: ${en[key]}\n  ${lang.code}: ${merged[key]}`);
  }
  for (const key of Object.keys(merged)) if (!(key in en)) throw new Error(`${lang.code}/${id}: ${key} is not an English key`);
}

// ---- blog posts --------------------------------------------------------------

const BLOG = `${I18N}/blog`;
const FROM_ENGLISH = ['date', 'published', 'updated', 'lastmod', 'navDocs'];
export function readPost(file) {
  const src = readFileSync(file, 'utf8');
  const m = /^---\n([\s\S]*?)\n---\n/.exec(src);
  if (!m) throw new Error(`${file}: no front matter`);
  const fields = {};
  for (const line of m[1].split('\n')) {
    const i = line.indexOf(': ');
    if (i < 0) throw new Error(`${file}: bad front matter line "${line}"`);
    fields[line.slice(0, i)] = line.slice(i + 2);
  }
  return { ...fields, body: src.slice(m[0].length) };
}
export function posts() {
  return readdirSync(BLOG).filter(slug => existsSync(`${BLOG}/${slug}/en.html`)).map(slug => {
    const en = readPost(`${BLOG}/${slug}/en.html`);
    const by = { en };
    for (const lang of languages.slice(1)) {
      const file = `${BLOG}/${slug}/${lang.code}.html`;
      if (!existsSync(file)) continue;
      const post = readPost(file);
      for (const f of FROM_ENGLISH) post[f] = en[f];
      for (const f of Object.keys(en)) if (!(f in post)) throw new Error(`${slug}/${lang.code}: missing ${f}`);
      if (skeleton(post.body) !== skeleton(en.body)) throw new Error(`${slug}/${lang.code}: the article markup differs from English`);
      by[lang.code] = post;
    }
    return { slug, out: `blog/${slug}/index.html`, by };
  }).sort((a, b) => b.by.en.published.localeCompare(a.by.en.published));
}

// ---- links -------------------------------------------------------------------

/** Paths that exist in every language, so links to them follow the reader's language. */
function localized(pages) {
  const set = new Set(pages.map(p => pathOf(p.out)));
  set.add('/blog/feed.xml');
  return set;
}
/** Point links to localized pages at the same page under `lang`'s prefix. */
function relink(html, lang, local) {
  if (!lang.prefix) return html;
  const edits = [];
  for (const n of walk(parse(html))) {
    if (n.type !== 'element' || n.attrs.some(a => a.name === 'hreflang')) continue;
    for (const a of n.attrs) {
      if (!['href', 'content', 'action'].includes(a.name) || !a.value) continue;
      const abs = a.value.startsWith(origin + '/');
      const url = abs ? a.value.slice(origin.length) : a.value;
      if (!url.startsWith('/') || url.startsWith('//')) continue;
      const path = url.replace(/[?#].*$/, '');
      if (!local.has(path) && !documents[path.slice(1)]?.locales[lang.code]) continue;
      edits.push({ start: a.valueStart, end: a.valueEnd, text: (abs ? origin : '') + lang.prefix + url });
    }
  }
  let out = '', at = 0;
  for (const e of edits.sort((x, y) => x.start - y.start)) { out += html.slice(at, e.start) + e.text; at = e.end; }
  return out + html.slice(at);
}

// ---- language chrome ---------------------------------------------------------

const escape = s => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');
function alternates(path, have) {
  const link = (code, lang) => `\n<link rel="alternate" hreflang="${code}" href="${origin}${lang.prefix}${path}" />`;
  return languages.filter(l => have.has(l.code)).map(l => link(l.code, l)).join('') + (have.has('en') ? link('x-default', EN) : '');
}
function switcher(lang, path, have, style, fallback) {
  // The 404 page has no counterpart to land on: its switcher goes to each home.
  const href = l => l.prefix + (style === 'links' ? '/' : have.has(l.code) ? path : fallback);
  const c = stringsFor(lang, 'common');
  if (style === 'links') {
    const links = languages.map(l => `<a href="${href(l)}" hreflang="${l.code}" lang="${l.code}"${l === lang ? ' aria-current="true"' : ''}>${l.label}</a>`);
    return `    <p class="lang-links" aria-label="${escape(c['common.language'])}">${links.join(' · ')}</p>\n`;
  }
  const items = languages.map(l => `          <li><a href="${href(l)}" hreflang="${l.code}" lang="${l.code}"${l === lang ? ' aria-current="true"' : ''}>${l.label}</a></li>\n`);
  return `      <details class="lang-switch">\n        <summary aria-label="Language — ${escape(c['common.language'])}: ${lang.label}">Language</summary>\n        <ul>\n${items.join('')}        </ul>\n      </details>\n`;
}
function values(lang, path, have, page) {
  return {
    lang: lang.code,
    alternates: alternates(path, have),
    switcher: switcher(lang, path, have, page.switcher, page.fallback ?? '/'),
    i18nScript: lang.prefix ? `<script src="/js/i18n/${lang.code}.js"></script>\n<script src="/js/demo-i18n.js"></script>\n` : '',
    waitlistLang: lang.prefix ? `\n        <input type="hidden" name="lang" value="${lang.code}" />` : '',
  };
}

// ---- outputs -----------------------------------------------------------------

export function build() {
  const files = new Map();
  const write = (out, text) => { if (files.has(out)) throw new Error(`${out} is written twice`); files.set(out, text); };
  const all = posts();
  const pages = [...site.pages, ...all.map(p => ({ id: 'blog-post', out: p.out }))];
  const local = localized(pages);
  const everyone = new Set(languages.map(l => l.code));

  for (const [path, doc] of Object.entries(documents)) {
    const hash = createHash('sha256').update(readFileSync(`${PUBLIC}/${path}`)).digest('hex');
    if (hash !== doc.source_sha256) throw new Error(`${path}: English source changed; update its translation and source hash`);
    for (const [code, source] of Object.entries(doc.locales)) {
      const lang = languages.find(l => l.code === code);
      if (!lang || lang === EN) throw new Error(`${path}: unsupported document locale ${code}`);
      write(outOf(lang, path), readFileSync(`${I18N}/documents/${source}`, 'utf8'));
    }
  }

  for (const page of site.pages) {
    const template = readFileSync(`${I18N}/templates/${page.id}.html`, 'utf8');
    const path = pathOf(page.out);
    for (const lang of languages) {
      const extra = page.id === 'blog-index' ? { cards: cards(lang, all) } : {};
      const html = render(template, stringsFor(lang, page.id), `${lang.code}/${page.id}`, { ...values(lang, path, everyone, page), ...extra });
      write(outOf(lang, page.out), relink(html, lang, local));
    }
  }
  const shell = readFileSync(`${I18N}/templates/blog-post.html`, 'utf8');
  for (const post of all) {
    const path = pathOf(post.out);
    const have = new Set(Object.keys(post.by));
    for (const lang of languages) {
      const f = post.by[lang.code];
      if (!f) continue;
      const html = render(shell, stringsFor(lang, 'blog-index'), `${lang.code}/blog/${post.slug}`, {
        ...values(lang, path, have, { fallback: '/blog/' }),
        title: f.title, description: f.description, ogTitle: f.ogTitle ?? f.title, ogDescription: f.ogDescription ?? f.description,
        canonical: `${origin}${path}`, heading: f.heading, date: formatDate(lang, f.date), kind: f.kind, body: f.body,
        navDocs: f.navDocs === 'yes',
      });
      write(outOf(lang, post.out), relink(html, lang, local));
    }
  }
  for (const lang of languages) write(outOf(lang, 'blog/feed.xml'), feed(lang, all));
  write('sitemap.xml', sitemap(all));
  for (const lang of languages.slice(1)) write(`js/i18n/${lang.code}.js`, script(lang));
  write('../src/messages.ts', workerMessages());
  return files;
}

function cards(lang, all) {
  const read = stringsFor(lang, 'blog-index')['blog-index.card.read'];
  return all.filter(p => p.by[lang.code]).map(p => {
    const f = p.by[lang.code];
    return `<a class="post-card" href="${lang.prefix}${pathOf(p.out)}">\n  <p class="post-card__date">${f.date.replaceAll('-', ' · ')}</p>\n  <p class="post-card__title">${f.cardTitle ?? f.heading}</p>\n  <p class="post-card__excerpt">${f.excerpt}</p>\n  <p class="post-card__read">${read}</p>\n</a>\n`;
  }).join('\n');
}

function feed(lang, all) {
  const s = stringsFor(lang, 'blog-index');
  const mine = all.filter(p => p.by[lang.code]);
  const base = `${origin}${lang.prefix}/blog/`;
  const updated = mine.map(p => p.by.en.updated).sort().at(-1);
  const entries = mine.map(p => {
    const f = p.by[lang.code];
    const url = `${origin}${lang.prefix}${pathOf(p.out)}`;
    return `  <entry>\n    <title>${f.feedTitle ?? f.heading}</title>\n    <id>${url}</id>\n    <link href="${url}" />\n    <published>${f.published}</published>\n    <updated>${f.updated}</updated>\n    <summary>${f.summary}</summary>\n  </entry>\n`;
  });
  const xmlLang = lang.prefix ? ` xml:lang="${lang.code}"` : '';
  return `<?xml version="1.0" encoding="utf-8"?>\n<feed xmlns="http://www.w3.org/2005/Atom"${xmlLang}>\n  <title>${s['common.openwalkietalkie-blog']}</title>\n  <subtitle>${s['blog-index.feed.subtitle']}</subtitle>\n  <id>${base}</id>\n  <link href="${base}" />\n  <link rel="self" type="application/atom+xml" href="${base}feed.xml" />\n  <updated>${updated}</updated>\n  <author><name>${site.author}</name></author>\n${entries.join('')}</feed>\n`;
}

function sitemap(all) {
  const rows = [
    ...site.pages.filter(p => p.sitemap !== false).map(p => ({ path: pathOf(p.out), lastmod: p.lastmod, changefreq: p.changefreq, priority: p.priority, have: languages.map(l => l.code) })),
    ...all.map(p => ({ path: pathOf(p.out), lastmod: p.by.en.lastmod, have: Object.keys(p.by) })),
  ];
  let xml = '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">\n';
  for (const row of rows) {
    const present = languages.filter(l => row.have.includes(l.code));
    const links = present.map(l => `    <xhtml:link rel="alternate" hreflang="${l.code}" href="${origin}${l.prefix}${row.path}" />\n`).join('') +
      `    <xhtml:link rel="alternate" hreflang="x-default" href="${origin}${row.path}" />\n`;
    for (const lang of present) {
      xml += `  <url>\n    <loc>${origin}${lang.prefix}${row.path}</loc>\n    <lastmod>${row.lastmod}</lastmod>\n`;
      if (row.changefreq) xml += `    <changefreq>${row.changefreq}</changefreq>\n`;
      if (row.priority) xml += `    <priority>${row.priority}</priority>\n`;
      xml += links + '  </url>\n';
    }
  }
  return xml + '</urlset>\n';
}

/** The Worker's waitlist replies, in every language, for src/worker.ts. */
function workerMessages() {
  const en = table(EN, 'worker');
  const all = Object.fromEntries(languages.map(lang => {
    const mine = table(lang, 'worker');
    for (const key of Object.keys(en)) if (!(key in mine)) throw new Error(`${lang.code}/worker: missing ${key}`);
    return [lang.code, { prefix: lang.prefix, ...Object.fromEntries(Object.keys(en).map(k => [k, mine[k]])) }];
  }));
  return `// Generated by tools/i18n/build.mjs from i18n/strings/<lang>/worker.json. Do not edit.\n` +
    `export const MESSAGES = ${JSON.stringify(all, null, 2)} as const;\n\n` +
    `export type Lang = keyof typeof MESSAGES;\n` +
    `export type Message = Exclude<keyof (typeof MESSAGES)["en"], "prefix">;\n`;
}

function script(lang) {
  const en = table(EN, 'js'), mine = table(lang, 'js');
  for (const key of Object.keys(en)) if (!(key in mine)) throw new Error(`${lang.code}/js: missing "${key}"`);
  const map = Object.fromEntries(Object.keys(en).map(k => [en[k], mine[k]]));
  return `// Generated by tools/i18n/build.mjs from i18n/strings/${lang.code}/js.json. Do not edit.\n` +
    `window.OWT_LANG = ${JSON.stringify({ code: lang.code, prefix: lang.prefix })};\n` +
    `window.OWT_STRINGS = ${JSON.stringify(map, null, 1)};\n` +
    `window.OWT_DEMO_STRINGS = ${JSON.stringify(readJson(`${I18N}/runtime/${lang.code}.json`), null, 1)};\n` +
    `window.OWT_DEMO_PATTERNS = ${JSON.stringify(readJson(`${I18N}/runtime/${lang.code}-patterns.json`))};\n`;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const check = process.argv.includes('--check');
  const files = build();
  let changed = 0;
  for (const [out, text] of files) {
    const file = `${PUBLIC}/${out}`;
    if (existsSync(file) && readFileSync(file, 'utf8') === text) continue;
    changed++;
    if (check) { console.error(`out of date: public/${out}`); continue; }
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  console.log(`${files.size} files, ${changed} ${check ? 'out of date' : 'written'}`);
  if (check && changed) process.exit(1);
}
