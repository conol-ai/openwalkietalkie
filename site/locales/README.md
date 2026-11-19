# Full-site localization — draft implementation

Add English, Japanese, Simplified Chinese and Traditional Chinese to the entire public website. This draft establishes the translation inventory, shared terminology and starter catalogs. It is **not ready to merge as a finished localization feature**: translated routes and the language selector are not enabled yet.

## Scope

The current inventory covers **28 HTML pages and 1,251 distinct messages**, including page titles, descriptions, accessible labels and image descriptions:

- Home page and the illustrated field manual.
- Documentation index, native companion API, Omarchy and Windows guides.
- Blog index and all 17 published articles.
- Privacy policy and cookie information.
- Waitlist confirmation and the 404 page.

Runtime strings also need localization in `main.js`, `arcade.js`, `privacy.js`, `waitlist.js` and the waitlist responses in `src/worker.ts`. These are tracked separately from the HTML inventory; the HTML coverage count does not imply runtime coverage.

## Language and URL contract

| Language | BCP 47 tag | URL prefix | Selector label |
| --- | --- | --- | --- |
| English | `en` | `/` | English |
| Japanese | `ja` | `/ja/` | 日本語 |
| Simplified Chinese | `zh-Hans` | `/zh-Hans/` | 简体中文 |
| Traditional Chinese | `zh-Hant` | `/zh-Hant/` | 繁體中文 |

Keep English at its existing URLs. Every translated page must have a matching language URL, including nested documentation, blog posts and legal pages. Switching language must preserve the current page and fragment when a translation exists. Internal links must keep the selected language. Use URL-based selection initially, without introducing a new persistent language preference or automatic redirect.

Generate static localized HTML so content and navigation work without JavaScript. Set `html[lang]`, translated titles and descriptions, canonical URLs, reciprocal `hreflang` links, and `x-default`. Extend the sitemap. Keep original code, command lines, API keys, endpoints, package names and external URLs intact.

## Catalogs

`en.json` is generated from the current English HTML. Each stable content key stores its source text and the pages that use it. Inline markup is retained so translators can change word order while keeping emphasis and links. Source changes create a new key, preventing an older translation from silently matching changed copy.

The other catalogs map those keys to translated text. Each currently contains **62 starter translations** for navigation, shared controls, metadata and selected home-page copy. Missing entries are intentionally absent, not copied from English and marked complete. Every language still needs the remaining body copy and contextual review.

From `site/`:

```sh
python3 tools/localize.py                 # Refresh the English inventory
python3 tools/localize.py --check         # Detect stale source inventory
python3 tools/localize.py --coverage      # Per-language, per-page progress
python3 tools/localize.py --require-complete  # Release gate; currently fails by design
python3 -m unittest discover -s tests -p 'test_localize.py'
```

The extractor excludes scripts, styles, code blocks, keyboard legends and `translate="no"` content. It includes accessible text and search/social metadata. The future renderer must use trusted checked-in catalogs, preserve HTML structure, and validate links and image references after generation.

## Terminology

| English | Japanese | Simplified Chinese | Traditional Chinese |
| --- | --- | --- | --- |
| User Manual | ユーザーマニュアル | 使用说明书 | 使用手冊 |
| Waitlist | 入荷通知 | 到货通知 | 到貨通知 |
| Push to Talk | プッシュ・トゥ・トーク | 按键通话 | 按鍵通話 |
| Scroll wheel | スクロールホイール | 滚轮 | 滾輪 |
| Firmware | ファームウェア | 固件 | 韌體 |
| Macropad | マクロパッド | 宏键盘 | 巨集鍵盤 |
| Speech recognition | 音声認識 | 语音识别 | 語音辨識 |
| Spoken replies | 音声での応答 | 语音回复 | 語音回覆 |
| Shortcut | ショートカット | 快捷键 | 快速鍵 |
| Save | 保存 | 保存 | 儲存 |

Keep OpenWalkieTalkie, Codex, Claude Code, Conol, Omarchy, CoHDL and GitHub as product names. In the manual, pair translated instructions with the exact English app labels, for example `音声（Voice）` or `语音（Voice）`. Keep screenshot content authentic and translate its surrounding explanation and accessible description. Preserve literal hardware key labels such as PTT, ENTER, CANCEL and SHIFT.

Traditional Chinese needs its own terminology review; character conversion alone is not completion. Japanese headings must read naturally rather than copy English word order.

## Completion checklist

- [x] Inventory all existing HTML pages, shared copy and accessible metadata.
- [x] Define the four locales, URL scheme and shared terminology.
- [x] Add Japanese, Simplified Chinese and Traditional Chinese starter catalogs.
- [x] Add inventory freshness, coverage and completeness checks.
- [ ] Translate and review the remaining home-page and manual content.
- [ ] Translate all documentation, blog articles, legal pages and utility pages.
- [ ] Localize dynamic demo, game, privacy notice and waitlist messages, including non-JavaScript form responses.
- [ ] Implement static localized output, cross-language links and an accessible language selector.
- [ ] Add CJK typography that preserves the pixel visual style and readable body text.
- [ ] Verify day/night device colorways, mobile navigation and all language layouts.
- [ ] Verify manual printing in all four languages; allow extra pages where translation requires them.
- [ ] Verify links, canonical/hreflang metadata, sitemap, waitlist behavior and translation completeness.
- [ ] Review legal translations against the English meaning without introducing new promises or changing consent.

Keep this PR as a draft until these items are complete. No production language switch should advertise partially translated pages as complete.
