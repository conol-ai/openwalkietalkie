#!/usr/bin/env node
// Turn finished English pages into templates and English string tables.
//
//   node tools/i18n/extract.mjs <id>=<page under public/> [...]
//   node tools/i18n/extract.mjs support=support/index.html
//
// Writes i18n/templates/<id>.html (the page with each piece of copy replaced
// by {{key}}) and i18n/strings/en/<id>.json. Copy that already appears in
// i18n/strings/en/common.json, or on two or more of the pages given, goes to
// common.json instead. The English page rebuilt from its template must match
// the input byte for byte, or nothing is written.
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { parse, walk, attr } from './html.mjs';
import { SITE, render, readJson, writeJson } from './lib.mjs';

const INLINE = new Set(['a', 'abbr', 'b', 'br', 'cite', 'code', 'em', 'i', 'img', 'kbd', 'mark', 'q', 's', 'samp', 'small', 'span', 'strong', 'sub', 'sup', 'time', 'u', 'var', 'wbr']);
const SKIP = new Set(['script', 'style', 'pre', 'svg', 'textarea', 'template', 'noscript']);
const CODEY = new Set(['code', 'kbd', 'samp']);
const TEXT_ATTRS = new Set(['alt', 'title', 'aria-label', 'placeholder']);
const META = /^(description|og:title|og:description|og:image:alt|twitter:title|twitter:description)$/;
const LETTER = /\p{L}/u;

const inlineOnly = n => n.type === 'text' || n.type === 'comment' || (n.type === 'element' && INLINE.has(n.name) && n.children.every(inlineOnly));
function prose(src, n) {
  if (n.type === 'text') return src.slice(n.start, n.end);
  if (n.type !== 'element' || CODEY.has(n.name)) return '';
  return n.children.map(c => prose(src, c)).join('');
}

/** Copy units of one page: [{ start, end, tag, attr?, ctx }], non-overlapping, in order. */
export function units(src) {
  const root = parse(src);
  const out = [];
  const ctx = n => { for (let p = n; p; p = p.parent) { const id = attr(p, 'id'); if (id) return id; if (p.name === 'head') return 'head'; } return 'page'; };
  const visit = el => {
    if (el.type !== 'element' && el.type !== 'root') return;
    if (SKIP.has(el.name)) return;
    if (el.name === 'title') {
      const t = el.children[0];
      if (t && LETTER.test(src.slice(t.start, t.end))) out.push({ start: t.start, end: t.end, tag: 'title', ctx: 'head' });
      return;
    }
    let run = [];
    const flush = () => {
      // A run of elements with no text between them (nav links, buttons, tags)
      // is several pieces of copy, not one sentence.
      if (run.length > 1 && run.every(n => n.type !== 'text' || !src.slice(n.start, n.end).trim())) {
        const parts = run; run = [];
        for (const n of parts) if (n.type === 'element') { run = [n]; flush(); }
        return;
      }
      // Leave markup without copy (icons, empty spans) in the template.
      // Punctuation next to the copy stays with it: Chinese and Japanese end with 。
      const bare = n => n.type === 'comment' || (n.type === 'text' ? !src.slice(n.start, n.end).trim() : !prose(src, n).trim());
      if (!run.some(n => LETTER.test(prose(src, n)))) run = [];
      while (run.length && bare(run[0])) run.shift();
      while (run.length && bare(run[run.length - 1])) run.pop();
      if (run.length) {
        let start = run[0].start, end = run[run.length - 1].end;
        // Trim surrounding whitespace, then unwrap a single wrapping inline element.
        while (/\s/.test(src[start])) start++;
        while (/\s/.test(src[end - 1])) end--;
        let only = run.length === 1 ? run[0] : null;
        while (only && only.type === 'element' && only.children.length && only.start === start && only.end === end) {
          start = only.innerStart; end = only.innerEnd;
          while (/\s/.test(src[start])) start++;
          while (/\s/.test(src[end - 1])) end--;
          const kids = only.children.filter(c => !(c.type === 'text' && !src.slice(c.start, c.end).trim()));
          only = kids.length === 1 ? kids[0] : null;
        }
        out.push({ start, end, tag: el.name, ctx: ctx(el) });
      }
      run = [];
    };
    for (const child of el.children) {
      if (inlineOnly(child)) { run.push(child); continue; }
      flush();
      visit(child);
    }
    flush();
  };
  visit(root);
  const inUnit = pos => out.some(u => u.start <= pos && pos < u.end);
  for (const n of walk(root)) {
    if (n.type !== 'element') continue;
    if ([...ancestors(n)].some(p => SKIP.has(p.name))) continue;
    for (const a of n.attrs) {
      if (a.value === null || !LETTER.test(a.value) || inUnit(a.valueStart)) continue;
      const meta = n.name === 'meta' && a.name === 'content' && META.test(attr(n, 'name') ?? attr(n, 'property') ?? '');
      const link = n.name === 'link' && a.name === 'title';
      if (!meta && !link && (!TEXT_ATTRS.has(a.name) || n.name === 'link')) continue;
      out.push({ start: a.valueStart, end: a.valueEnd, tag: n.name, attr: meta ? 'content' : a.name, ctx: ctx(n) });
    }
  }
  return out.sort((a, b) => a.start - b.start);
}
function* ancestors(n) { for (let p = n; p; p = p.parent) yield p; }

const slug = s => s.replace(/<[^>]*>/g, '').replace(/&[a-z]+;/g, '').normalize('NFKD').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'text';

if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2).map(a => a.split('='));
  if (!args.length || args.some(a => a.length !== 2)) { console.error('usage: extract.mjs <id>=<path under public/> ...'); process.exit(2); }
  const commonFile = `${SITE}/i18n/strings/en/common.json`;
  const common = existsSync(commonFile) ? readJson(commonFile) : {};
  const byValue = new Map(Object.entries(common).map(([k, v]) => [v, k]));
  const pages = args.map(([id, path]) => {
    const src = readFileSync(`${SITE}/public/${path}`, 'utf8');
    return { id, path, src, units: units(src).map(u => ({ ...u, value: src.slice(u.start, u.end) })) };
  });
  // Copy on two or more pages becomes common.
  const seen = new Map();
  for (const p of pages) for (const v of new Set(p.units.map(u => u.value))) seen.set(v, (seen.get(v) ?? 0) + 1);
  const used = new Set(Object.keys(common));
  for (const [v, count] of seen) {
    if (count < 2 || byValue.has(v)) continue;
    let key = `common.${slug(v)}`;
    for (let i = 2; used.has(key); i++) key = `common.${slug(v)}-${i}`;
    used.add(key); common[key] = v; byValue.set(v, key);
  }
  mkdirSync(`${SITE}/i18n/templates`, { recursive: true });
  for (const p of pages) {
    const strings = {};
    const keyOf = new Map();
    const counters = new Map();
    let template = '', at = 0;
    for (const u of p.units) {
      let key = byValue.get(u.value) ?? keyOf.get(u.value);
      if (!key) {
        const base = `${p.id}.${u.ctx}.${u.tag}${u.attr ? '-' + u.attr : ''}`;
        const n = (counters.get(base) ?? 0) + 1; counters.set(base, n);
        key = `${base}${n}`; keyOf.set(u.value, key); strings[key] = u.value;
      }
      template += p.src.slice(at, u.start) + `{{${key}}}`; at = u.end;
    }
    template += p.src.slice(at);
    const back = render(template, { ...common, ...strings }, `${p.id} (roundtrip)`);
    if (back !== p.src) throw new Error(`${p.id}: template does not rebuild the English page`);
    p.template = template; p.strings = strings;
  }
  for (const p of pages) {
    writeFileSync(`${SITE}/i18n/templates/${p.id}.html`, p.template);
    writeJson(`${SITE}/i18n/strings/en/${p.id}.json`, p.strings);
    console.log(`${p.id}: ${Object.keys(p.strings).length} strings, ${p.units.length} uses`);
  }
  writeJson(commonFile, common);
  console.log(`common: ${Object.keys(common).length} strings`);
}
