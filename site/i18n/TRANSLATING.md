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
