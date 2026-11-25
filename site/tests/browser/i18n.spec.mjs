import { test, expect } from '@playwright/test';

const LANGS = [['zh-Hans', '简体中文'], ['zh-Hant', '繁體中文'], ['ja', '日本語']];

test('language homes and blog indexes are served from their index files', async ({ page }) => {
  for (const [code] of LANGS) {
    for (const path of [`/${code}/`, `/${code}/blog/`, `/${code}/docs/omarchy/`, `/${code}/blog/feed.xml`]) {
      const result = await page.request.get(path);
      expect(result.status(), path).toBe(200);
    }
    // Without the trailing slash, the asset handler redirects to it.
    const bare = await page.request.get(`/${code}`, { maxRedirects: 0 });
    expect(bare.status()).toBe(307);
    expect(bare.headers().location).toBe(`/${code}/`);
    await page.goto(`/${code}/`);
    await expect(page.locator('html')).toHaveAttribute('lang', code);
  }
});

test('a missing page under a language prefix gets that language’s 404 page', async ({ page }) => {
  for (const [code] of LANGS) {
    const result = await page.goto(`/${code}/no-such-channel/`);
    expect(result.status()).toBe(404);
    await expect(page.locator('html')).toHaveAttribute('lang', code);
    await expect(page.locator('.lang-links a[aria-current]')).toHaveAttribute('hreflang', code);
  }
  const english = await page.goto('/no-such-channel/');
  expect(english.status()).toBe(404);
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
});

test('the edge switcher lands on the same page in the chosen language', async ({ page }) => {
  await page.goto('/blog/radio-branding/');
  await page.getByRole('button', { name: 'Got it' }).click();
  for (const [code, label] of LANGS) {
    await page.locator('.lang-switch summary').click();
    await page.locator('.lang-switch').getByRole('link', { name: label }).click();
    await expect(page).toHaveURL(new RegExp(`/${code}/blog/radio-branding/$`));
    await expect(page.locator('html')).toHaveAttribute('lang', code);
  }
  await page.locator('.lang-switch summary').click();
  await page.locator('.lang-switch').getByRole('link', { name: 'English' }).click();
  await expect(page).toHaveURL(/\/blog\/radio-branding\/$/);
});

test('mobile: the switcher stays available without opening the menu and the page does not overflow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  for (const path of ['/', '/ja/', '/zh-Hant/manual/']) {
    await page.goto(path);
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), path).toBe(true);
  }
  await expect(page.locator('.lang-switch summary')).toBeVisible();
  await page.locator('.lang-switch summary').click();
  await expect(page.locator('.lang-switch').getByRole('link', { name: 'English' })).toBeVisible();
});

test('a translated page localises the privacy notice and the waitlist replies', async ({ page }) => {
  await page.goto('/ja/#waitlist');
  const notice = page.locator('.privacy-notice');
  await expect(notice).toBeVisible();
  await expect(notice.locator('a').first()).toHaveAttribute('href', '/ja/privacy/');
  await notice.locator('button').click();
  await page.locator('#waitlist-form input[type="email"]').fill(`ja-${crypto.randomUUID()}@example.com`);
  await page.locator('#waitlist-consent').check();
  await page.locator('#waitlist-form button').click();
  await expect(page.locator('#waitlist-status')).toHaveAttribute('data-state', 'success');
  await expect(page.locator('#waitlist-status')).not.toContainText("You're on the list");
});

test('without JavaScript a translated form lands on the translated thanks page', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:8790/zh-Hant/#waitlist');
  await page.locator('#waitlist-form input[type="email"]').fill(`zh-${crypto.randomUUID()}@example.com`);
  await page.locator('#waitlist-consent').check();
  await page.locator('#waitlist-form button').click();
  await expect(page).toHaveURL(/\/zh-Hant\/waitlist\/thanks\/$/);
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-Hant');
  await context.close();
});
