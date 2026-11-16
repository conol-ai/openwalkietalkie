import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// The home page's markup without its comments, which no visitor sees.
const home = readFileSync(new URL('../public/index.html', import.meta.url), 'utf8').replace(/<!--[\s\S]*?-->/g, '');

// The markup of the <section> with this id, including any sections nested in it.
function section(id) {
  const open = new RegExp(`<section\\b[^>]*\\bid="${id}"[^>]*>`).exec(home);
  assert.ok(open, `the home page has a section with id "${id}"`);
  const tags = /<(\/?)section\b[^>]*>/g;
  tags.lastIndex = open.index + open[0].length;
  for (let depth = 1, tag; (tag = tags.exec(home));) {
    depth += tag[1] ? -1 : 1;
    if (depth === 0) return home.slice(open.index, tags.lastIndex);
  }
  assert.fail(`section "${id}" is never closed`);
}
const links = html => [...html.matchAll(/<a\b([^>]*)>([\s\S]*?)<\/a>/g)].map(([, attrs, text]) => ({
  href: /\bhref="([^"]*)"/.exec(attrs)?.[1] ?? '', text: text.replace(/<[^>]*>/g, '').trim()
}));
// A way to the Mac app: the Download section, a release or its disk image, or a link labelled as a download.
const getsTheApp = link => /#download$|\/releases\b|\.dmg\b/.test(link.href) || /download/i.test(link.text);

test('the hero offers the waitlist and no Mac download while hardware is waitlist-only', () => {
  const hero = links(section('hero'));
  assert.deepEqual(hero.find(link => link.href === '#waitlist'), { href: '#waitlist', text: 'Join the waitlist' });
  assert.deepEqual(hero.filter(getsTheApp), []);
});

test('the Download section keeps both versioned Mac downloads', () => {
  const release = /^https:\/\/github\.com\/conol-ai\/openwalkietalkie\/releases\/download\/v(\d+\.\d+\.\d+)\/OpenWalkieTalkie-\1-macos-(aarch64|x86_64)\.dmg$/;
  // Found through the hero's check, so that check cannot pass by recognising nothing.
  const downloads = links(section('download')).filter(getsTheApp).map(link => release.exec(link.href)).filter(Boolean);
  assert.deepEqual(downloads.map(([, , arch]) => arch).sort(), ['aarch64', 'x86_64']);
  assert.equal(new Set(downloads.map(([, version]) => version)).size, 1, 'both downloads name the same release');
});

test('the Download section offers the Omarchy guide, which walks through the whole install', () => {
  const omarchy = links(section('download')).filter(link => link.href === '/docs/omarchy/');
  assert.ok(omarchy.some(link => /omarchy/i.test(link.text)), 'a button names Omarchy');
  const guide = readFileSync(new URL('../public/docs/omarchy/index.html', import.meta.url), 'utf8');
  for (const step of ['pacman -S', 'cargo build --release --target x86_64-unknown-linux-gnu -p deckd',
    'omarchy/install.sh', '70-openwalkietalkie.rules', 'omarchy_hook.py install claude', 'superwhisper',
    '71-openwalkietalkie-uinput.rules', 'usage.providers', 'uninstall all']) {
    assert.ok(guide.includes(step), `the guide covers ${step}`);
  }
  const docs = readFileSync(new URL('../public/docs/index.html', import.meta.url), 'utf8');
  assert.ok(docs.includes('href="/docs/omarchy/"'), 'the docs index links the guide');
  const sitemap = readFileSync(new URL('../public/sitemap.xml', import.meta.url), 'utf8');
  assert.ok(sitemap.includes('https://openwalkietalkie.org/docs/omarchy/'), 'the sitemap lists the guide');
});

test('the Download section offers the Windows ZIP and its unsigned-preview install guide', () => {
  const zip = links(section('download')).find(link => /windows-x86_64\.zip$/.test(link.href));
  assert.ok(zip && /windows/i.test(zip.text), 'a download button names Windows');
  assert.match(zip.href, /^https:\/\/github\.com\/conol-ai\/openwalkietalkie\/releases\/download\//);
  const windows = links(section('download')).filter(link => link.href === '/docs/windows/');
  assert.ok(windows.some(link => /windows/i.test(link.text)), 'a button names Windows');
  assert.ok(!links(section('hero')).some(link => link.href === '/docs/windows/'), 'the hero stays waitlist-only');
  const guide = readFileSync(new URL('../public/docs/windows/index.html', import.meta.url), 'utf8');
  assert.ok(guide.includes('href="https://github.com/conol-ai/openwalkietalkie/releases/latest"'), 'the guide links the latest release');
  assert.match(guide, /OpenWalkieTalkie-&lt;version&gt;-windows-x86_64\.zip/, 'the guide names the release asset');
  for (const step of ['More info', 'Run anyway', 'Smart App Control', 'Start']) {
    assert.ok(guide.includes(step), `the guide covers ${step}`);
  }
});
