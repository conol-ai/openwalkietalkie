import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

export const SITE = fileURLToPath(new URL('../..', import.meta.url)).replace(/\/$/, '');
export const readJson = file => JSON.parse(readFileSync(file, 'utf8'));
export function writeJson(file, value) {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, JSON.stringify(value, null, 2) + '\n');
}

/**
 * Fill a template. {{key}} is a string from `strings`; {{=name}} is a value the
 * build computes (`values`); {{#name}}…{{/name}} keeps its body only when that
 * value is truthy. A missing key or value is an error, never an empty string.
 */
export function render(template, strings, where, values = {}) {
  const out = template.replace(/\{\{#([\w-]+)\}\}([\s\S]*?)\{\{\/\1\}\}/g, (_, name, body) => {
    if (!(name in values)) throw new Error(`${where}: no value for {{#${name}}}`);
    return values[name] ? body : '';
  });
  return out.replace(/\{\{(=?)([a-z0-9][\w.@-]*)\}\}/gi, (_, computed, key) => {
    const table = computed ? values : strings;
    if (!(key in table)) throw new Error(`${where}: no ${computed ? 'value' : 'string'} for {{${computed}${key}}}`);
    return table[key];
  });
}
