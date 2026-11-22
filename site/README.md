# openwalkietalkie.org

The official OpenWalkieTalkie website, sharing OpenMicroKbd's pixel-art day
and night theme. It includes an interactive deck, product photos, firmware and
hardware renders, Mac downloads, release posts with an Atom feed, and a
waitlist for people interested in buying a finished device.

## Preview

Install the development tools with Node.js 24 or newer, then start a local
Worker and SQLite database. Local signups stay on your machine:

```bash
npm ci
npm run db:init:local
npm run dev
```

Pages are generated from `i18n/` (see [Languages](#languages)); everything
they produce is committed, so previewing needs no build. A plain file server
can preview the design, but cannot process waitlist submissions:

```bash
python3 -m http.server 8787 -d public
```

## Deploy

The site uses Cloudflare Workers static assets with a Worker for `/api/*`.
Waitlist emails live in a dedicated D1 database, separate from Cohdl's list.
The `wrangler.jsonc` routes provision HTTPS for `openwalkietalkie.org` and
`www.openwalkietalkie.org` on the CoNoL Cloudflare account.

```bash
cd site
npx wrangler login --browser=false  # only when the login has expired
npm run db:init
npm run build:i18n -- --check
npm run types
npm run typecheck
npm test
npm run test:browser
npx wrangler deploy --dry-run
npm run deploy
```

Open the printed login URL in the browser profile that holds the Cloudflare
account. Both hostnames serve the site; canonical URLs use the apex domain.
The workers.dev URL serves the same deployment and database as production.
Browser tests launch a local Worker and use installed Google Chrome.

For a new Cloudflare account, create a database with
`npx wrangler d1 create openwalkietalkie-site`, put its ID in `wrangler.jsonc`,
and apply migrations before deploying. The rate-limit namespace must be
unique within that account. Regenerate binding types after config changes.

## Waitlist and privacy

The home page links to `#waitlist`. Its email form posts to `/api/waitlist`
and requires explicit permission for device availability emails. It works
without JavaScript, including confirmation and error pages. JSON submissions
show the result in place. Successful duplicate signups receive the same
response without adding another row or changing the original consent date.

The Worker validates and normalizes addresses, bounds request bodies,
rejects cross-origin form posts, uses a honeypot and a short IP-based rate
limit, and binds SQL parameters. The database contains only email, signup
time, and consent version. Application logs contain no submitted fields.
Cloudflare still handles technical connection data for hosting and security.

No emails or payments are sent by this feature. Before sending availability
messages, export the list through an authorized account and include a way
to leave it. Do not commit exports or put them in public assets:

```bash
npm run waitlist:count
npm run --silent waitlist:export > /private/tmp/openwalkietalkie-waitlist.json
```

Handle removal requests through `privacy@cohdl.ai`, the existing COHDL
privacy contact. Delete the matching normalized email using a bound SQL
parameter; do not interpolate user input into SQL. A daily scheduled Worker
also deletes signups older than two years. Cloudflare backup retention is
separate from deletion in the active list.

`public/privacy/` and `public/cookies/` describe this behavior. The notice is
informational: no advertising or analytics are configured. `owt-theme`
stores the chosen theme in local storage; `owt-privacy-notice` remembers
dismissal for 180 days. Footer links reopen the notice, and the pages remain
available without JavaScript. If tracking, embedded media, data processors,
or waitlist purposes change, update the disclosure and consent behavior
before enabling them. Bump the notice version when it should be shown again.

## Languages

The site speaks the app's four languages: English at the root (`/`,
`/blog/x/`), and 简体中文, 繁體中文 and 日本語 under `/zh-Hans/`, `/zh-Hant/`
and `/ja/`. Every page exists in all four, with `hreflang` alternates, a
language menu in the header, a feed per language (`/ja/blog/feed.xml`) and one
sitemap listing them all. Nothing is translated twice by hand: one template per
page, one string table per language, one generator.

```
i18n/
  site.json                  languages, product pages, sitemap dates
  templates/<page>.html      a page's markup; each piece of copy is {{key}}
  templates/blog-post.html   the shell every post shares
  strings/<lang>/<page>.json the copy of one page; common.json for header and footer
  strings/<lang>/js.json     what public/js/*.js shows (→ public/js/i18n/<lang>.js)
  strings/<lang>/worker.json the waitlist API's replies (→ src/messages.ts)
  blog/<slug>/<lang>.html    a post: front matter, then the article
  GLOSSARY.md                the app's terminology in each language
  TRANSLATING.md             voice, markup and punctuation rules for translators
tools/i18n/
  build.mjs                  writes the pages, feeds, sitemap, js/i18n and src/messages.ts
  extract.mjs                turns a finished English page into a template + table
  check.mjs                  checks one translation without building
```

Rebuild after any change under `i18n/`, and commit `public/` with it:

```bash
npm run build:i18n           # writes public/; a second run changes nothing
npm run build:i18n -- --check  # fails if public/ is not what i18n/ builds
```

The build fails on a missing key, a missing language, or a translation whose
markup (tags, `href`, `src`, classes) differs from English. English pages come
out byte for byte as written, plus their alternates and the language menu.
Links to the site's own pages are pointed at the reader's language by the
build; assets (`/css/`, `/assets/`, `/fonts/`, `/js/`, `/examples/`, the API
reference) stay shared. Do not edit generated files under `public/` (the
pages, `blog/feed.xml`, `sitemap.xml`, `js/i18n/`) or `src/messages.ts`.

**Change copy:** edit `i18n/strings/en/<page>.json` and the same key in the
three other languages, then rebuild. **Change markup:** edit the template.

**Add a page:** write it in English under `public/` as before, then

```bash
node tools/i18n/extract.mjs support=support/index.html
```

writes `i18n/templates/support.html` and `i18n/strings/en/support.json`
(copy already in `common.json` is reused). Add `{{=lang}}`, `{{=alternates}}`,
`{{=switcher}}` and `{{=i18nScript}}` where the other templates have them (the
`<html>` tag, after the canonical link, before the header's closing `</nav>`,
before the first script), add the page to `i18n/site.json`, translate the
table into each language (`node tools/i18n/check.mjs strings ja support`) and
rebuild. `tests/i18n.test.mjs` fails until every English page has its three
counterparts.

**Add a blog post:** write `i18n/blog/<slug>/en.html` (copy a recent post for
the front matter) and its `zh-Hans.html`, `zh-Hant.html` and `ja.html`, then
rebuild. The build adds it to each blog index, feed and the sitemap; nothing
under `public/blog/` is edited by hand any more.

**Add a language:** add it to `languages` in `i18n/site.json` (code, URL
prefix, its own name), copy `i18n/strings/en/` to `i18n/strings/<code>/` and
translate every table and post, add the code to `LANGS` in
`tests/i18n.test.mjs` and `tests/browser/i18n.spec.mjs`, extend `GLOSSARY.md`
from the app's tables, then rebuild. The menu, alternates, feeds and the
Worker's replies follow from `site.json`. Use the date format in
`formatDate` in `tools/i18n/build.mjs` if the language needs another one.

`tests/i18n.test.mjs` checks that `public/` matches the source, that every
page has all four versions with a complete and reciprocal `hreflang` set and a
working language menu, that every link and asset resolves without leaving the
reader's language, that translated pages carry none of a set of English
sentinel sentences, and that feeds and the sitemap list everything.
`tests/browser/i18n.spec.mjs` checks routing, the per-language 404 page, the
menu and the translated waitlist on a local Worker.

## Content and releases

- `public/index.html`: product information, interactive deck, gallery, downloads.
  While the deck is waitlist-only, the hero offers the waitlist, the browser
  demo and the source, but no Mac download; the nav, the Blueprint section and
  the Download section still lead to it. `tests/home.test.mjs` checks the hero
  and both versioned Mac downloads; update it when the deck goes on sale.
- `public/js/main.js`: theme, demo, PTT simulation, and galleries. The browser
  demo uses scripted text; it does not record audio or connect to real agents.
- `src/worker.ts` and `migrations/`: waitlist API and retention task.
- `public/js/waitlist.js`: progressive enhancement for the signup form.
- `public/js/privacy.js`: dismissible privacy notice on all pages.
- `public/docs/`: native companion guide and generated JSON-RPC reference.
  `public/examples/native_companion.py` must match `host/examples/native_companion.py`.
- `i18n/blog/`: posts in every language; `npm run build:i18n` writes
  `public/blog/`, the Atom feeds and `sitemap.xml` from them.
- `public/assets/screens/`: firmware renders from `tools/preview-all.py`.
- `public/assets/photos/`: product photos (hero, the hardware stack and the
  two colourway cards), generated by `sh tools/photos.sh` from the studio set
  in `../ref/OpenWalkieTalkie`. The script auto-orients, crops to the window
  each card shows, resizes for 2x displays and strips all metadata; the
  originals stay out of `site/public`.
- `public/assets/og.jpg`: the social card, rendered by `node tools/og.mjs`
  from `tools/og/og.html` (the hero photo and the site's own fonts) with the
  installed Google Chrome.
- `public/_headers`: security headers and caching. Mutable CSS and JavaScript
  revalidate on each visit, so a deployment does not leave stale behavior.

GitHub releases and the website are published separately. The release
workflow does not generate blog posts or deploy this Worker. For each release:

1. Add a post under `i18n/blog/<slug>/` based on `docs/releases/<version>.md`,
   linking to that version's GitHub release, in all four languages.
2. Run `npm run build:i18n`. It adds the post to every blog index, feed and
   the sitemap, newest first. Update the page dates in `i18n/site.json` when
   a page changes.
3. Update the home page's release copy (`i18n/strings/*/home.json`) and both
   versioned Mac download URLs (`i18n/templates/home.html`), then rebuild. Versioned URLs keep old website copies
   working after a newer release.
4. Include those changes in the release commit before tagging. The existing
   `python3 tools/release.py validate v<version>` release check now rejects
   a missing post, feed entry, index link, sitemap entry, or Mac download.
5. For 0.11.0 and later, include the native companion guide, docs navigation,
   sitemap entries, downloadable Python example and generated API reference.
   Release validation checks these too. Regenerate the reference with
   `deckd api-doc > docs/deckd-api.md` and copy it to
   `site/public/docs/deckd-api.md`; copy the canonical example from
   `host/examples/native_companion.py` after changes. Preview the guide and test
   the example before tagging. These prepared pages are published with 0.11.0,
   not as a separate early deployment.
6. After the GitHub release and its downloads are public, run the deployment
   checks above and deploy the site. Verify the live blog, feed, both post
   links, and the home page downloads. Passing release validation does not
   publish the website. For 0.11.0, also verify `/docs/`,
   `/docs/native-companions/`, the example download and the API reference on the
   live site.

The film player is dormant until film markup and a YouTube ID are added.
No analytics property is configured.

## Licenses

Site code uses the repository's MIT license. The bundled Press Start 2P and
VT323 fonts keep their SIL Open Font License notices in `public/fonts/`.
Hardware renders come from the repository's CERN-OHL-P-2.0 hardware designs;
the corresponding notice is included as `public/assets/HARDWARE-LICENSE.txt`.

## V9.3B website design preview

The homepage retains the original white/orange day and black/mint night pixel
illustrations, floating animation, mouse parallax and theme transition. Only the
screen pixels are replaced: the day screen shows the supplied agent roster and
the night screen shows the supplied push-to-talk listening view. Each screenshot
uses a corner-pin mapping aligned with its illustrated glass edges, including
the bottom hint row, and a nearest-neighbor pixel texture to match the artwork.
The original enclosure pixels and alpha silhouettes are preserved. The existing
CSS alignment keeps the visible device size and position consistent across themes.
The navigation's GitHub link opens the on-page Blueprint section.

Try It exposes Agents, Omarchy, Macropad, Usage, System and Arcade. Workspace
selection and macro editing operate on sample data. Arcade provides browser
adaptations of Flappy Bird, Snake and Raiden; it does not run firmware code.
The preview never accesses the microphone, posts OS shortcuts or connects
to deckd. Timer and Settings are omitted. Shared description rows keep the
activity display stationary when changing faces.

The Faces is an indexed screen atlas, separate from the existing hardware
photo carousel. Its eighth image is the supplied 1920px Snake firmware
screenshot. The hardware carousel and both colourway cards are preserved.

Use the static server command above to review front-end interactions. Waitlist
submission still requires the Worker/D1 development server.
