# Translating the website

The site is written in English and translated into 简体中文 (`zh-Hans`),
繁體中文 (`zh-Hant`) and 日本語 (`ja`), the four languages the app and the
deck already speak. This file is for whoever writes or reviews a translation.

## Where the copy lives

- `strings/<lang>/<page>.json`: the copy of one product page, one entry per
  paragraph, heading, button or label. `common.json` holds what every page
  shares (header, footer). The keys are the same in every language; the
  English file is the source.
- `strings/<lang>/js.json`: what the site's scripts write into the page (the
  privacy notice, the waitlist form's messages, the browser demo's labels).
  Keep `{0}` and `{1}` placeholders.
- `strings/<lang>/worker.json`: the waitlist API's replies.
- `blog/<slug>/<lang>.html`: one blog post. Front matter between the `---`
  lines, then the article's markup.

Check a file as you go, without building the site:

```bash
node tools/i18n/check.mjs strings zh-Hant home
node tools/i18n/check.mjs post ja radio-branding
```

## Terminology

Read `GLOSSARY.md` first. It lists the words the app and the deck already use,
copied from their string tables. Use those words. A reader who installs the
app after reading the site should see the same term in both places. If a term
is not in the glossary, search the app tables it points to before inventing
one.

- **zh-Hant is Taiwan usage, written the way the app writes it.** Not zh-Hans
  through a character converter, and not mainland vocabulary in Traditional
  characters: 裝置, 韌體, 連線, 檔案, 程式, 設定, 預設, 資料, 軟體, 硬體,
  網站, 部落格, 影片, 支援, 訊息, 視窗.
- **zh-Hans** uses mainland usage: 设备, 固件, 连接, 文件, 程序, 设置.
- **ja** uses です／ます in prose. Short labels and headings may be noun
  phrases, as in the app.

## Voice

The English is plain: short sentences, concrete facts, no marketing. Keep it
that way. Translate sentence by sentence; do not add, drop, or soften claims.
Do not explain what the English does not explain. If a sentence is a fragment
in English ("Fast agents. Slow you."), a short fragment is right.

Headings set in the pixel font are in capitals in English. Chinese and
Japanese have no capitals; translate the words and keep them short.

## Leave these as they are

- Product and project names: OpenWalkieTalkie, Open Walkie Talkie, deckd,
  Conol, COHDL / CoHDL, Claude, Claude Code, Codex, Grok Bot, Omarchy,
  Hyprland, macOS, Windows, Linux, GitHub, Discord, Apple Silicon, Intel,
  Whisper, Superwhisper, KiCad, FreeCAD.
- Commands, code, file names, paths, URLs, email addresses, version numbers,
  config keys, JSON, anything inside `<code>`, `<kbd>`, `<pre>`, `<samp>`.
- The names of the deck's physical keys: PTT, ENTER, ESC, SHIFT, CANCEL.
- Units and numbers (480×480, 16 kHz, USB-C).
- The brand mark text `OPEN WALKIE TALKIE` and the theme labels' pixel art.

When the English names a menu, button or page of the app or the deck, use
that item's name in the app's language (from the glossary), not a fresh
translation, so the reader can find it. Where the app has no translation for a
label (it shows English), keep the English label.

## Markup

Every value keeps the English markup exactly: the same tags, in the same
order, with the same `href`, `src`, `class` and other attributes. Only the
text changes, and the text attributes `alt`, `title`, `aria-label`,
`placeholder` and meta `content`. Do not add `<br>`, emphasis or links. The
build and `check.mjs` reject a value whose markup differs from English.

Keep HTML entities as entities (`&amp;`, `&lt;`). In JSON, escape `"` as
`\"`. Typographic quotes inside prose follow the language:

- zh-Hans: “…” and ‘…’
- zh-Hant and ja: 「…」 and 『…』

Use full-width punctuation in Chinese and Japanese prose (，。：；！？（）)
and put a space between CJK text and Latin words or numbers
(`在 Mac 上`, `0.15.0 版`), as the app does. No space next to full-width
punctuation.

Dates are written by the build. Where a date appears inside copy, write it as
`2026年10月16日` in all three languages.

## Blog posts

Copy the English file and translate it in place. In the front matter,
translate `title`, `description`, `heading`, `kind`, `summary`, `excerpt`, and
`ogTitle`, `ogDescription`, `feedTitle` or `cardTitle` where they exist. Leave
`date`, `published`, `updated`, `lastmod` and `navDocs` as they are; the build
takes them from English anyway. Each field stays on one line.

Titles keep the version: `OpenWalkieTalkie v0.15.0：在 Omarchy 上……` (a
full-width colon in Chinese and Japanese). The `title` field keeps its
` · OpenWalkieTalkie` suffix.

The post's markup, images and links stay exactly as in English; links to the
site's own pages are pointed at the reader's language by the build.

A few values begin right after an identifier that stays in the page, for
example `<code>cmake</code> {{key}}` ("and `clang` build the local speech
recogniser…"). Write those so the sentence reads with the identifier as its
subject: `<code>cmake</code> 和 <code>clang</code> 用于…`,
`<code>cmake</code> と <code>clang</code> は…`.

## Product-context review, 2026-11-24

The user requested a fresh review of all three translations before submitting
a new PR. This revision starts from the current main branch (609ec6e) and uses
its i18n source structure. It does not merge the withdrawn inventory PR or the
older preview branch. Release-specific articles retain their historical context;
current download information remains at 0.17.0.

For each language, an independent model first back-translated 1,375 numbered
segments without the English source. Only after saving that pass did it compare
every segment against English, the product glossary, the firmware and companion
service documentation, and the actual website demo handlers. Corrections include
held versus momentary presses, PTT elapsed recording time, the number of ENTER
presses, computer-side speech recognition, usage-limit periods, API access,
firmware update conditions and the scope of privacy promises. Final corrections
were not independently back-translated a second time. This is not native review.

The illustrated manual follows an English software interface: its quoted menu
and button names match the screenshots. Explanatory text and the hardware button
diagram are localized, including nine final accessibility descriptions per locale
checked during browser review (model review, not a separate blind pass). Physical control names, commands, code, project identifiers,
version numbers and authentic screenshots remain intact. The original pixel art,
day/night models and animations are preserved.

| Language | Batch | Translated / reviewed by | Review | Date | Notes |
|---|---|---|---|---|---|
| zh-Hans (你) | 1,375 static page/blog segments | Codex and independent blind model pass | back-translated | 2026-11-24 | 203 corrections in the recorded comparison; final integration also reconciles source corrections. |
| zh-Hant (Taiwan, 你) | 1,375 static page/blog segments | Codex and independent blind model pass | back-translated | 2026-11-24 | 253 corrections, including previously imported diagram labels. Translated from English, not character-converted from Simplified Chinese. |
| ja (です／ます in prose) | 1,375 static page/blog segments | Codex and independent blind model pass | back-translated | 2026-11-24 | 191 corrections, including previously imported diagram labels. Final diagram wording shortened to fit its annotation lines. |
| zh-Hans / zh-Hant / ja | 201 runtime strings and 46 dynamic patterns per language | Codex and separate contextual model passes | machine | 2026-11-24 | 192 strings plus all patterns reviewed in the separate pass; 9 final strings cover composed activity messages and literal physical controls. Placeholders and commands checked. |
| zh-Hans | 230 unique API-reference segments | Codex and prior independent blind pass, followed by current-source comparison | back-translated | 2026-11-24 | Rechecked against current schema and ten added method rows; 52 lines corrected. |
| zh-Hant / ja | 230 unique API-reference segments per language | Model translation from current English and structural checks | machine | 2026-11-24 | All 106 methods, 92 named types, code and schema fields preserved. |

Mechanical review preserves deliberate English brands, identifiers, schema types
and the English controls shown in screenshots. Those identical technical strings
are accepted warnings. The generic printf checker misreads the English prose
`51.5% fewer` as `% f`; the comparison fixture escapes that percent only for this
check, while the site's actual text keeps the original percentage. No target
prose or protocol value is changed to satisfy that false positive. Policy pages
were checked against the source, not reviewed by a native-language legal editor.

The localized API references live under `documents/<locale>/`. Their shared
manifest pins the English source SHA-256: the build stops when it changes, so
update every translation and the hash together. Do not edit the generated English
API reference to fix a translation. Runtime catalogs live under `runtime/`;
`demo-i18n.js` translates their variable activity messages without altering user
commands or adding a language-storage preference.
