#!/usr/bin/env node
// Check one translation against English without building anything.
//
//   node tools/i18n/check.mjs strings <lang> <id>     i18n/strings/<lang>/<id>.json
//   node tools/i18n/check.mjs post <lang> <slug>      i18n/blog/<slug>/<lang>.html
//
// Same keys or front-matter fields as English, the same markup in every
// value, and a warning for any value that is still the English sentence.
import { readFileSync } from 'node:fs';
import { SITE, readJson } from './lib.mjs';
import { skeleton, readPost } from './build.mjs';

const [kind, lang, id] = process.argv.slice(2);
if (!['strings', 'post'].includes(kind) || !lang || !id) { console.error('usage: check.mjs strings|post <lang> <id|slug>'); process.exit(2); }
const errors = [], warnings = [];
// Three or more English words in a row, outside code, is probably untranslated.
const english = html => /\b[A-Za-z]{2,}[a-z]* [a-z]{2,} [a-z]{2,}\b/.test(html.replace(/<(code|kbd|pre|samp)\b[\s\S]*?<\/\1>/g, '').replace(/<[^>]*>/g, ' ').replace(/https?:\S+/g, ''));
function compare(en, mine, where) {
  for (const key of Object.keys(en)) {
    if (!(key in mine)) { errors.push(`${where}: missing ${key}`); continue; }
    if (typeof mine[key] !== 'string') { errors.push(`${where}: ${key} is not a string`); continue; }
    try {
      if (skeleton(en[key]) !== skeleton(mine[key])) errors.push(`${where}: ${key} changes the markup\n    en: ${en[key]}\n    ${lang}: ${mine[key]}`);
    } catch (e) { errors.push(`${where}: ${key} does not parse: ${e.message}`); }
    if (lang !== 'en' && english(mine[key]) && mine[key] === en[key]) warnings.push(`${where}: ${key} is still English: ${mine[key].slice(0, 90)}`);
  }
  for (const key of Object.keys(mine)) if (!(key in en)) errors.push(`${where}: ${key} is not an English key`);
}
if (kind === 'strings') {
  const base = `${SITE}/i18n/strings`;
  let mine;
  try { mine = readJson(`${base}/${lang}/${id}.json`); } catch (e) { errors.push(`${lang}/${id}.json: ${e.message}`); }
  if (mine) {
    const en = readJson(`${base}/en/${id}.json`);
    if (id === 'js' || id === 'worker') {
      for (const k of Object.keys(en)) if (!(k in mine)) errors.push(`${id}: missing ${k}`);
      for (const k of Object.keys(mine)) if (!(k in en)) errors.push(`${id}: ${k} is not an English key`);
      for (const k of Object.keys(en)) for (const p of en[k].match(/\{\d\}/g) ?? []) if (!mine[k]?.includes(p)) errors.push(`${id}: ${k} lost ${p}`);
    } else compare(en, mine, `${lang}/${id}`);
  }
} else {
  const dir = `${SITE}/i18n/blog/${id}`;
  try {
    const en = readPost(`${dir}/en.html`);
    const mine = readPost(`${dir}/${lang}.html`);
    compare(en, mine, `${id}/${lang}`);
  } catch (e) { errors.push(e.message); }
}
for (const w of warnings) console.log(`warning: ${w}`);
for (const e of errors) console.log(`error: ${e}`);
console.log(errors.length ? `${errors.length} errors` : 'ok');
process.exit(errors.length ? 1 : 0);
