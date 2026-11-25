import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

function translator(locale = 'zh-Hans') {
  const root = new URL('../', import.meta.url);
  const catalog = JSON.parse(readFileSync(new URL(`i18n/runtime/${locale}.json`, root)));
  const patterns = JSON.parse(readFileSync(new URL(`i18n/runtime/${locale}-patterns.json`, root)));
  const context = {
    window: { OWT_DEMO_STRINGS: catalog, OWT_DEMO_PATTERNS: patterns },
  };
  vm.runInNewContext(readFileSync(new URL('public/js/demo-i18n.js', root), 'utf8'), context);
  return context.window.OWTI18n.t;
}

test('composite demo activity translates descriptions and preserves project names', () => {
  const t = translator();
  assert.equal(t('HOST → DECK · card · Codex · cohdl-api asks a question'), '电脑 → 对讲机 · 卡片 · Codex · cohdl-api 提出问题');
  assert.equal(t('prompt delivered · Codex · cohdl-api is working'), '指令已送达 · Codex · cohdl-api 正在运行');
  assert.equal(t('Key 3 saved · Screenshot · cmd+shift+4'), '按键 3 已保存 · Screenshot · cmd+shift+4');
  assert.equal(t('0' + 3 + ' / MACROPAD'), '03 / 宏键盘');
});

for (const locale of ['zh-Hans', 'zh-Hant', 'ja']) {
  test(`${locale}: demo copy preserves placeholders, identifiers and shortcuts`, () => {
    const load = name => JSON.parse(readFileSync(new URL(`../i18n/runtime/${name}.json`, import.meta.url)));
    const canonical = load('zh-Hans');
    const catalog = load(locale);
    assert.deepEqual(Object.keys(catalog).sort(), Object.keys(canonical).sort());
    const slots = value => (value.match(/\{\d+\}/g) || []).sort();
    for (const [source, target] of Object.entries(catalog)) assert.deepEqual(slots(target), slots(source), source);
    const patterns = load(`${locale}-patterns`);
    assert.deepEqual(patterns.map(p => p[0]), load('zh-Hans-patterns').map(p => p[0]));
    for (let i = 0; i < patterns.length; i++) {
      assert.deepEqual(patterns[i][1].match(/\$\d+/g), load('zh-Hans-patterns')[i][1].match(/\$\d+/g));
      assert.doesNotThrow(() => new RegExp(patterns[i][0]));
    }
    const t = translator(locale);
    for (const value of ['PTT', 'ENTER', 'SHIFT', 'LEFT', 'RIGHT', 'cmd+shift+4', 'cohdl-api', 'main-revb']) assert.equal(t(value), value);
    assert.notEqual(t('Run migrations on staging?'), 'Run migrations on staging?');
    assert.ok(t('Key 3 saved · Screenshot · cmd+shift+4').includes('Screenshot · cmd+shift+4'));
    assert.ok(t('prompt delivered · Codex · cohdl-api is working').includes('Codex · cohdl-api'));
    assert.notEqual(t('Flappy Bird · local browser game · PTT controls play'), 'Flappy Bird · local browser game · PTT controls play');
  });
}

test('already translated text and literal controls remain stable', () => {
  const t = translator();
  for (const text of ['LEFT', 'RIGHT', 'ENTER', 'PTT', 'cmd+shift+4', '直接和你的 AI 助手对话。']) assert.equal(t(text), text);
  const text=t('prompt delivered · Codex · cohdl-api is working');
  assert.equal(t(text), text);
});
