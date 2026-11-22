import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { readFileSync } from 'node:fs';
import worker from '../src/worker.ts';

function fixture(t) {
  const db = new DatabaseSync(':memory:');
  db.exec(readFileSync(new URL('../migrations/0001_waitlist.sql', import.meta.url), 'utf8'));
  t.after(() => db.close());
  const keys = [];
  return { db, keys, env: {
    DB: { prepare(sql) { return { bind(...args) { return { async run() { return db.prepare(sql).run(...args); } }; } }; } },
    WAITLIST_LIMITER: { async limit({ key }) { keys.push(key); return { success: true }; } },
    ASSETS: { async fetch() { return new Response('asset'); } }
  } };
}
function request(fields = {}, headers = {}) {
  return new Request('https://openwalkietalkie.org/api/waitlist', {
    method: 'POST', headers: { 'content-type': 'application/json', origin: 'https://openwalkietalkie.org', ...headers },
    body: JSON.stringify({ email: 'person@example.com', consent: true, ...fields })
  });
}
const count = db => db.prepare('SELECT COUNT(*) AS n FROM waitlist').get().n;

test('normalises emails, persists consent, and gives duplicates the same result', async t => {
  const { env, db } = fixture(t);
  const first = await worker.fetch(request({ email: '  Person@Example.com  ' }), env);
  const duplicate = await worker.fetch(request(), env);
  assert.equal(first.status, 200);
  assert.deepEqual(await first.json(), await duplicate.json());
  assert.equal(count(db), 1);
  const row = db.prepare('SELECT * FROM waitlist').get();
  assert.deepEqual(Object.keys(row), ['email', 'created_at', 'consent_version']);
  assert.equal(row.email, 'person@example.com');
  assert.equal(row.consent_version, '2026-10-16');
  assert.ok(Number.isFinite(Date.parse(row.created_at)));
  assert.equal(first.headers.get('cache-control'), 'no-store');
});

test('parameter binding safely stores a valid address containing an apostrophe', async t => {
  const { env, db } = fixture(t);
  const email = "o'reilly+device@example.com";
  assert.equal((await worker.fetch(request({ email }), env)).status, 200);
  assert.equal(db.prepare('SELECT email FROM waitlist').get().email, email);
});

for (const email of ['', 'not-an-email', 'a@b', 'a@@b.com', '.a@example.com', 'a..b@example.com', 'a.@example.com', 'a\r\nb@example.com', 'a@-bad.com', 'a@bad-.com', 'x'.repeat(65) + '@example.com', 42, null]) {
  test(`rejects invalid address ${JSON.stringify(email)}`, async t => {
    const { env, db } = fixture(t);
    const result = await worker.fetch(request({ email }), env);
    assert.equal(result.status, 400);
    assert.equal((await result.json()).error, 'invalid_email');
    assert.equal(count(db), 0);
  });
}

test('requires an explicit availability-email opt-in', async t => {
  const { env, db } = fixture(t);
  for (const consent of [false, null, 'false', 'true', 1]) {
    const result = await worker.fetch(request({ consent }), env);
    assert.equal(result.status, 400);
    assert.equal((await result.json()).error, 'consent_required');
  }
  assert.equal(count(db), 0);
});

test('honeypot submissions appear successful without being stored', async t => {
  const { env, db } = fixture(t);
  assert.equal((await worker.fetch(request({ company: 'bot' }), env)).status, 200);
  assert.equal(count(db), 0);
});

test('cross-site and malformed origins are rejected without throwing', async t => {
  const { env, db } = fixture(t);
  for (const origin of ['null', 'broken url', 'https://evil.example', 'http://openwalkietalkie.org']) {
    assert.equal((await worker.fetch(request({}, { origin }), env)).status, 403);
  }
  assert.equal((await worker.fetch(request({}, { 'sec-fetch-site': 'cross-site' }), env)).status, 403);
  assert.equal(count(db), 0);
});

test('limits actual streamed bytes even without a Content-Length header', async t => {
  const { env, db } = fixture(t);
  const req = new Request('https://openwalkietalkie.org/api/waitlist', {
    method: 'POST', duplex: 'half', headers: { 'content-type': 'application/json' },
    body: new ReadableStream({ start(controller) {
      controller.enqueue(new TextEncoder().encode(' '.repeat(4096)));
      controller.enqueue(new TextEncoder().encode('{}'));
      controller.close();
    } })
  });
  assert.equal((await worker.fetch(req, env)).status, 413);
  assert.equal(count(db), 0);
});

test('rejects invalid JSON, null and arrays without touching storage', async t => {
  const { env, db } = fixture(t);
  for (const body of ['{', 'null', '[]', '"x"']) {
    const req = new Request('https://openwalkietalkie.org/api/waitlist', {
      method: 'POST', headers: { 'content-type': 'application/json' }, body
    });
    assert.equal((await worker.fetch(req, env)).status, 400);
  }
  assert.equal(count(db), 0);
});

test('rate limits before storing and supplies a retry time', async t => {
  const { env, db } = fixture(t);
  env.WAITLIST_LIMITER.limit = async () => ({ success: false });
  const result = await worker.fetch(request(), env);
  assert.equal(result.status, 429);
  assert.equal(result.headers.get('retry-after'), '60');
  assert.equal(count(db), 0);
});

test('storage failure never reports a successful signup', async t => {
  const { env } = fixture(t);
  env.DB.prepare = () => { throw new Error('private database failure'); };
  const result = await worker.fetch(request(), env);
  assert.equal(result.status, 503);
  assert.doesNotMatch(await result.text(), /private|person@example/);
});

test('HTML form posts redirect to a real success page; errors are readable without JS', async t => {
  const { env, db } = fixture(t);
  const form = email => new Request('https://openwalkietalkie.org/api/waitlist', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ email, consent: 'yes' })
  });
  const result = await worker.fetch(form('person@example.com'), env);
  assert.equal(result.status, 303);
  assert.equal(result.headers.get('location'), '/waitlist/thanks/');
  assert.equal(count(db), 1);
  const bad = await worker.fetch(form('<script>'), env);
  assert.equal(bad.status, 400);
  assert.match(bad.headers.get('content-type'), /text\/html/);
  assert.match(await bad.text(), /Please enter a valid email address/);
});

test('routing and allowed methods do not mask API errors with HTML assets', async t => {
  const { env } = fixture(t);
  const get = await worker.fetch(new Request('https://openwalkietalkie.org/api/waitlist'), env);
  assert.equal(get.status, 405);
  assert.equal(get.headers.get('allow'), 'POST');
  assert.equal((await worker.fetch(request({}, { 'content-type': 'text/plain' }), env)).status, 415);
  assert.equal((await worker.fetch(new Request('https://openwalkietalkie.org/api/unknown'), env)).status, 404);
  assert.equal(await (await worker.fetch(new Request('https://openwalkietalkie.org/privacy/'), env)).text(), 'asset');
});

test('daily retention removes old signups and preserves recent ones', async t => {
  const { env, db } = fixture(t);
  const old = new Date(); old.setUTCFullYear(old.getUTCFullYear() - 3);
  db.prepare('INSERT INTO waitlist VALUES (?, ?, ?)').run('old@example.com', old.toISOString(), 'old');
  await worker.fetch(request(), env);
  await worker.scheduled({}, env);
  assert.deepEqual(db.prepare('SELECT email FROM waitlist').all().map(r => r.email), ['person@example.com']);
});

test('a translated page gets its language back: JSON messages, the thanks page and error pages', async t => {
  const { env, db } = fixture(t);
  const { MESSAGES } = await import('../src/messages.ts');
  for (const lang of ['zh-Hans', 'zh-Hant', 'ja']) {
    const ok = await worker.fetch(request({ email: `${lang}@example.com`, lang }), env);
    assert.deepEqual(await ok.json(), { ok: true, message: MESSAGES[lang].success });
    assert.notEqual(MESSAGES[lang].success, MESSAGES.en.success, `${lang} success message is translated`);
    const bad = await worker.fetch(request({ email: 'nope', lang }), env);
    assert.deepEqual(await bad.json(), { ok: false, error: 'invalid_email', message: MESSAGES[lang].invalid_email });
    const form = email => new Request('https://openwalkietalkie.org/api/waitlist', {
      method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', origin: 'https://openwalkietalkie.org' },
      body: new URLSearchParams({ email, consent: 'yes', lang }).toString()
    });
    const thanks = await worker.fetch(form(`form-${lang}@example.com`), env);
    assert.equal(thanks.status, 303);
    assert.equal(thanks.headers.get('location'), `/${lang}/waitlist/thanks/`);
    const page = await (await worker.fetch(form('<script>'), env)).text();
    assert.match(page, new RegExp(`<html lang="${lang}">`));
    assert.ok(page.includes(MESSAGES[lang].invalid_email) && page.includes(`href="/${lang}/#waitlist"`) && page.includes(`href="/${lang}/privacy/"`));
    assert.ok(!page.includes('<script>'));
  }
  assert.equal(count(db), 6);
});

test('an unknown or missing language falls back to English and never reaches the redirect', async t => {
  const { env } = fixture(t);
  for (const lang of [undefined, 'fr', '../evil', '__proto__', 'constructor', 42]) {
    const result = await worker.fetch(request({ email: 'x', lang }), env);
    assert.equal((await result.json()).message, 'Please enter a valid email address.');
  }
  const form = new Request('https://openwalkietalkie.org/api/waitlist', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', origin: 'https://openwalkietalkie.org' },
    body: 'email=person%40example.com&consent=yes&lang=%2F%2Fevil.example'
  });
  assert.equal((await worker.fetch(form, env)).headers.get('location'), '/waitlist/thanks/');
});
