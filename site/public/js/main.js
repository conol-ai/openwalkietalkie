/* ============================================================
   OPEN WALKIE TALKIE — page behaviour
   Vanilla JS, no deps. Same shape as openmicrokbd.org's main.js:
   IIFE-scoped modules, independent of each other.
   ============================================================ */
(function () {
  'use strict';

  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var $  = function (s, r) { return (r || document).querySelector(s); };
  var $$ = function (s, r) { return Array.prototype.slice.call((r || document).querySelectorAll(s)); };
  var clamp = function (v, a, b) { return v < a ? a : v > b ? b : v; };

  /* ------------------------------------------------------------
     1. THEME (day / night) — persisted, crossfaded via CSS
     theme-init.js already set data-theme before first paint.
     ------------------------------------------------------------ */
  (function theme() {
    var KEY = 'owt-theme';
    var root = document.documentElement;
    var btn = $('#themeToggle');
    var label = $('#themeLabel');

    apply(root.getAttribute('data-theme') === 'night' ? 'night' : 'day', true);

    function apply(mode, silent) {
      root.setAttribute('data-theme', mode);
      if (label) label.textContent = mode === 'night' ? 'NIGHT' : 'DAY';
      if (btn) btn.setAttribute('aria-pressed', mode === 'night' ? 'true' : 'false');
      $$('meta[name="theme-color"]').forEach(function (m) {
        m.setAttribute('content', mode === 'night' ? '#131a3a' : '#f2e2bd');
      });
      if (!silent) {
        try { localStorage.setItem(KEY, mode); } catch (e) {}
      }
    }

    if (btn) {
      btn.addEventListener('click', function () {
        apply(root.getAttribute('data-theme') === 'night' ? 'day' : 'night');
        window.OWT_blip && window.OWT_blip(520, 0.05);
      });
    }
  })();

  /* ------------------------------------------------------------
     2. STARS (night sky) — generated once, twinkle via CSS
     ------------------------------------------------------------ */
  (function stars() {
    var host = $('#stars');
    if (!host) return;
    var n = window.innerWidth < 700 ? 34 : 64;
    var frag = document.createDocumentFragment();
    for (var i = 0; i < n; i++) {
      var s = document.createElement('span');
      s.className = 'star' + (i % 9 === 0 ? ' star--big' : '');
      s.style.left = (Math.random() * 100).toFixed(2) + '%';
      s.style.top = (Math.random() * 62).toFixed(2) + '%';
      s.style.animationDelay = (Math.random() * 3.2).toFixed(2) + 's';
      frag.appendChild(s);
    }
    host.appendChild(frag);
  })();

  /* ------------------------------------------------------------
     3. NAV: mobile burger + hide-on-scroll-down
     ------------------------------------------------------------ */
  (function nav() {
    var burger = $('#navBurger');
    var links = $('.nav__links');
    var navEl = $('#nav');

    if (burger && links) {
      burger.addEventListener('click', function () {
        var open = links.classList.toggle('is-open');
        burger.setAttribute('aria-expanded', open ? 'true' : 'false');
      });
      links.addEventListener('click', function (e) {
        if (e.target.tagName === 'A') {
          links.classList.remove('is-open');
          burger.setAttribute('aria-expanded', 'false');
        }
      });
    }

    var last = 0;
    window.addEventListener('scroll', function () {
      var y = window.pageYOffset;
      if (navEl && !reduceMotion) {
        var hide = y > last && y > 220 && !(links && links.classList.contains('is-open'));
        navEl.style.transform = hide ? 'translateY(-140%)' : 'translateY(0)';
      }
      last = y;
    }, { passive: true });

    if (navEl) {
      navEl.addEventListener('focusin', function () {
        navEl.style.transform = 'translateY(0)';
      });
    }
  })();

  /* ------------------------------------------------------------
     4. SCROLL PROGRESS BAR + hero/cloud parallax
     ------------------------------------------------------------ */
  (function scrollFx() {
    var fill = $('#progressFill');
    var layers = $$('[data-parallax]');
    var heroArt = $('#heroArt');
    var ticking = false;

    function frame() {
      ticking = false;
      var y = window.pageYOffset;
      var max = document.documentElement.scrollHeight - window.innerHeight;
      if (fill) fill.style.width = (max > 0 ? clamp(y / max, 0, 1) * 100 : 0).toFixed(2) + '%';
      if (reduceMotion) return;
      for (var i = 0; i < layers.length; i++) {
        var r = parseFloat(layers[i].getAttribute('data-parallax')) || 0;
        layers[i].style.transform = 'translate3d(0,' + (y * r).toFixed(1) + 'px,0)';
      }
      if (heroArt && y < window.innerHeight * 1.4) {
        heroArt.style.setProperty('--pmy-scroll', (y * -0.06).toFixed(1) + 'px');
      }
    }

    window.addEventListener('scroll', function () {
      if (!ticking) { ticking = true; requestAnimationFrame(frame); }
    }, { passive: true });
    frame();
  })();

  /* ------------------------------------------------------------
     5. POINTER PARALLAX on the hero device
     ------------------------------------------------------------ */
  (function pointerParallax() {
    if (reduceMotion) return;
    var art = $('#heroArt');
    var hero = $('#hero');
    if (!art || !hero || window.matchMedia('(hover: none)').matches) return;

    hero.addEventListener('pointermove', function (e) {
      var r = hero.getBoundingClientRect();
      var nx = (e.clientX - r.left) / r.width - 0.5;
      var ny = (e.clientY - r.top) / r.height - 0.5;
      art.style.setProperty('--pmx', (nx * 26).toFixed(1) + 'px');
      art.style.setProperty('--pmy-tilt', (ny * 16).toFixed(1) + 'px');
    });
    hero.addEventListener('pointerleave', function () {
      art.style.setProperty('--pmx', '0px');
      art.style.setProperty('--pmy-tilt', '0px');
    });
  })();

  /* ------------------------------------------------------------
     6. SCROLL REVEALS (IntersectionObserver + stagger)
     ------------------------------------------------------------ */
  (function reveals() {
    var items = $$('.reveal');
    if (!items.length) return;

    if (reduceMotion || !('IntersectionObserver' in window)) {
      items.forEach(function (el) { el.classList.add('revealed'); });
      return;
    }

    $$('[data-stagger]').forEach(function (group) {
      $$('.reveal', group).forEach(function (child, i) {
        child.style.transitionDelay = (i * 90) + 'ms';
      });
    });

    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) {
          en.target.classList.add('revealed');
          io.unobserve(en.target);
        }
      });
    }, { threshold: 0.12, rootMargin: '0px 0px -8% 0px' });

    items.forEach(function (el) { io.observe(el); });
  })();

  /* ------------------------------------------------------------
     7. WEBAUDIO BLIP (square wave) + mute toggle
     ------------------------------------------------------------ */
  var audio = (function () {
    var ctx = null;
    var on = true;
    var Ctor = window.AudioContext || window.webkitAudioContext;

    function ensure() {
      if (!Ctor) return null;
      if (!ctx) { try { ctx = new Ctor(); } catch (e) { return null; } }
      if (ctx.state === 'suspended') ctx.resume();
      return ctx;
    }

    function blip(freq, dur, type) {
      if (!on) return;
      var c = ensure();
      if (!c) return;
      var t = c.currentTime;
      var osc = c.createOscillator();
      var g = c.createGain();
      osc.type = type || 'square';
      osc.frequency.setValueAtTime(freq || 660, t);
      g.gain.setValueAtTime(0.0001, t);
      g.gain.linearRampToValueAtTime(0.055, t + 0.006);
      g.gain.exponentialRampToValueAtTime(0.0001, t + (dur || 0.07));
      osc.connect(g).connect(c.destination);
      osc.start(t);
      osc.stop(t + (dur || 0.07) + 0.02);
    }

    var btn = $('#soundToggle');
    var label = $('#soundLabel');
    if (btn) {
      btn.addEventListener('click', function () {
        on = !on;
        btn.setAttribute('aria-pressed', on ? 'true' : 'false');
        if (label) label.textContent = 'SOUND: ' + (on ? 'ON' : 'OFF');
        if (on) blip(760, 0.06);
      });
    }

    window.OWT_blip = blip;
    return { blip: blip, isOn: function () { return on; } };
  })();

  /* ------------------------------------------------------------
     7b. A shared "type a transcript into an element" helper
     ------------------------------------------------------------ */
  function typer(el, text, done) {
    var timers = [];
    var n = 0;
    el.textContent = '';
    if (reduceMotion) { el.textContent = text; if (done) done(); return { stop: function () {} }; }
    (function step() {
      el.textContent = text.slice(0, n);
      n += 2;
      if (n <= text.length + 2) timers.push(setTimeout(step, 28));
      else if (done) done();
    })();
    return { stop: function () { timers.forEach(clearTimeout); } };
  }

  /* ------------------------------------------------------------
     7c. THE FEATURED PTT KEY
     Hold it (pointer, Space or Enter) and the little screen shows
     the LISTENING overlay; let go and the words are sent.
     ------------------------------------------------------------ */
  (function featuredPtt() {
    var btn = $('#featPtt');
    if (!btn) return;
    var out = $('#featPttOut');
    var overlay = $('#featPttOverlay');
    var text = $('#featPttText');
    var readout = $('#featPttReadout');
    var wave = $('#featPttWave');
    var LINE = 'Refactor the auth module, then run the test suite.';
    var held = false, t0 = 0, typing = null, closeTimer = null;

    if (wave) for (var i = 0; i < 22; i++) {
      var b = document.createElement('i');
      b.style.animationDelay = (i * 40) + 'ms';
      b.style.animationDuration = (380 + (i % 5) * 90) + 'ms';
      wave.appendChild(b);
    }

    function down() {
      if (held) return;
      held = true; t0 = Date.now();
      clearTimeout(closeTimer);
      btn.classList.add('is-held');
      if (out) out.classList.add('is-live');
      if (overlay) overlay.classList.add('is-on');
      if (readout) readout.textContent = 'Listening. The deck streams 16 kHz audio to the Mac while you hold the key.';
      if (text) typing = typer(text, LINE);
      audio.blip(520, 0.05);
    }
    function up() {
      if (!held) return;
      held = false;
      btn.classList.remove('is-held');
      if (typing) typing.stop();
      if (text) text.textContent = LINE;
      var secs = ((Date.now() - t0) / 1000).toFixed(1);
      if (readout) readout.textContent = 'Released after ' + secs + ' s. The transcript goes to the active agent as its next prompt.';
      audio.blip(760, 0.06);
      closeTimer = setTimeout(function () {
        if (overlay) overlay.classList.remove('is-on');
        if (out) out.classList.remove('is-live');
      }, 1600);
    }

    btn.addEventListener('pointerdown', function (e) { e.preventDefault(); try { btn.setPointerCapture(e.pointerId); } catch (err) {} down(); });
    ['pointerup', 'pointercancel'].forEach(function (ev) { btn.addEventListener(ev, up); });
    btn.addEventListener('keydown', function (e) { if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) { e.preventDefault(); down(); } });
    btn.addEventListener('keyup', function (e) { if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); up(); } });
    btn.addEventListener('blur', up);
  })();

  /* ------------------------------------------------------------
     8. THE REPLICA — the deck's grammar, in miniature.
        Faces: Agents · Omarchy · Macropad · Usage · System · Arcade.
        SHIFT+SCROLL moves between faces, SCROLL moves the
        highlight, ENTER opens, CANCEL goes back, PTT talks.
     ------------------------------------------------------------ */
  (function deck() {
    var deckEl = $('#deck');
    if (!deckEl) return;

    var FACES = ['AGENTS', 'OMARCHY', 'MACROPAD', 'USAGE', 'SYSTEM', 'ARCADE'];
    var faceEls = $$('.face', deckEl);
    var title = $('#screenTitle');
    var tag = $('#screenTag');
    var hint = $('#screenHint');
    var logEl = $('#hostLog');
    var lastEv = $('#lastEvent');
    var keys = $$('.dkey', deckEl);
    var wheel = $('#wheel');
    var drum = $('#wheelDrum');

    /* ---- demo data: the roster the demo host pushes ---- */
    var agents = [
      { kind: 'Codex', name: 'cohdl-api', state: 'q', sub: 'Run migrations on staging?', age: 2,
        q: { text: 'Tests fail on main. Run pending migrations on staging first?', choices: ['Run on staging', 'Skip migrations', 'Abort task'], rec: 0 } },
      { kind: 'Claude', name: 'web-ui', state: 'r', sub: 'Header refactor done · 34 files',
        ready: { text: 'Header refactor done, 34 files changed.', steps: ['Run the e2e suite', 'Open a pull request'] } },
      { kind: 'Codex', name: 'firmware', state: 'w', sub: 'Compiling openmicro · 2m' },
      { kind: 'Script', name: 'nightly', state: 'i', sub: 'Idle · via the open API' }
    ];
    var STATE_NAME = { q: 'Question', r: 'Ready', w: 'Working', e: 'Error', i: 'Idle' };

    var face = 0;
    var view = 'roster';          // roster | channel | card
    var hl = 0;                   // roster highlight
    var chan = 0;                 // active channel
    var choice = 0;
    var sent = false;
    var shiftHeld = false;
    var pttHeld = false;
    var usageHl = 0, wsHl = 0;
    var timers = [];
    function later(fn, ms) { timers.push(setTimeout(fn, ms)); }

    /* ---- host log ---- */
    function log(dir, text, cls) {
      if (!logEl) return;
      var li = document.createElement('li');
      li.className = (dir === 'in' ? 'in' : 'out') + (cls ? ' ' + cls : '');
      var b = document.createElement('b'); b.textContent = dir === 'in' ? '←' : '→';
      var s = document.createElement('span'); s.textContent = text;
      li.appendChild(b); li.appendChild(s);
      logEl.appendChild(li);
      while (logEl.children.length > 9) logEl.removeChild(logEl.firstChild);
      if (lastEv) lastEv.textContent = (dir === 'in' ? 'HOST → DECK · ' : 'DECK → HOST · ') + text;
    }

    /* ---- render: roster ---- */
    var rosterEl = $('#roster');
    function renderRoster() {
      if (!rosterEl) return;
      rosterEl.innerHTML = '';
      agents.forEach(function (a, i) {
        var li = document.createElement('li');
        li.className = 'agent' + (i === hl ? ' is-hl' : '') + (i === chan ? ' is-chan' : '');
        li.innerHTML = '<span class="agent__dot dot--' + a.state + '"></span><span><span class="agent__name">' +
          a.kind + ' · ' + a.name + '</span><br><span class="agent__sub">' + a.sub + '</span></span>';
        li.addEventListener('click', function () {
          if (hl === i) { openChannel(); } else { hl = i; chan = i; renderRoster(); log('out', 'channel ' + (i + 1) + ' · ' + a.kind + ' · ' + a.name); audio.blip(560, 0.04); }
        });
        rosterEl.appendChild(li);
      });
    }

    /* ---- render: channel + card ---- */
    var chanWho = $('#chanWho'), chanState = $('#chanState'), chanReply = $('#chanReply'), chanPending = $('#chanPending');
    var cardWho = $('#cardWho'), cardQ = $('#cardQ'), cardChoices = $('#cardChoices');

    function renderChannel() {
      var a = agents[chan];
      if (chanWho) chanWho.textContent = a.kind + ' · ' + a.name;
      if (chanState) chanState.textContent = STATE_NAME[a.state] + (a.age ? ' · ' + a.age + 'm' : '');
      if (chanReply) chanReply.textContent = a.q ? a.q.text : a.ready ? a.ready.text : a.sub;
      if (chanPending) {
        chanPending.innerHTML = '';
        if (a.q && a.q.choices.length === 2) {
          a.q.choices.forEach(function (c, i) {
            var d = document.createElement('div'); d.className = 'choice'; d.innerHTML = '<i>' + (i + 1) + '</i>' + c;
            chanPending.appendChild(d);
          });
        } else if (a.q) {
          var d1 = document.createElement('div'); d1.className = 'choice'; d1.textContent = 'Answer · ' + a.q.choices.length + ' choices'; chanPending.appendChild(d1);
        } else if (a.ready) {
          var d2 = document.createElement('div'); d2.className = 'choice'; d2.textContent = 'Ready summary'; chanPending.appendChild(d2);
        }
      }
    }

    function renderCard() {
      var a = agents[chan];
      if (cardWho) cardWho.textContent = a.kind + ' · ' + a.name;
      if (!cardChoices) return;
      cardChoices.innerHTML = '';
      var items = a.q ? a.q.choices : a.ready ? a.ready.steps : [];
      if (cardQ) cardQ.textContent = a.q ? a.q.text : a.ready ? a.ready.text : '';
      items.forEach(function (c, i) {
        var d = document.createElement('div');
        d.className = 'choice' + (i === choice ? ' is-hl' : '') + (a.q && a.q.rec === i && !sent ? ' is-rec' : '') + (sent && i === choice ? ' is-sent' : '');
        d.innerHTML = '<i>' + (i + 1) + '</i>' + c;
        d.addEventListener('click', function () { choice = i; renderCard(); confirmCard(); });
        cardChoices.appendChild(d);
      });
    }

    /* ---- macropad ---- */
    var MACROS = [
      ['build', '‹', 'left'], ['test', '›', 'right'], ['deploy', '↵', 'enter'],
      ['git push', 'SHIFT ‹', 'shift+left'], ['screenshot', '⇧⌘4', null], ['lock', '^⌘Q', null],
      ['mute', 'SHIFT ›', 'shift+right'], ['term', 'SHIFT ↵', 'shift+enter'], null
    ];
    var mgrid = $('#mgrid');
    var mkeys = [];
    var editingMacros = false, editingSlot = 0;
    var editor = $('#macroEditor');
    function renderMacros() {
      if (!mgrid) return;
      mgrid.replaceChildren(); mkeys = [];
      MACROS.forEach(function (m, i) {
        var d = document.createElement('button'); d.type = 'button';
        d.className = 'mkey' + (!m ? ' mkey--empty' : '');
        var small = document.createElement('small'); small.textContent = m ? m[1] : '';
        var label = document.createElement('span'); label.textContent = m ? m[0] : '+ assign';
        d.append(small, label);
        d.addEventListener('click', function () { if (!m || editingMacros || shiftHeld) openEditor(i); else pressMacro(i, 'touch'); });
        mgrid.appendChild(d); mkeys.push(d);
      });
    }
    function openEditor(i) {
      editingSlot = i;
      $('#macroEditorTitle').textContent = 'EDIT KEY ' + (i + 1);
      $('#macroLabel').value = MACROS[i] ? MACROS[i][0] : '';
      $('#macroShortcut').value = MACROS[i] ? MACROS[i][1] : '';
      editor.hidden = false; $('#macroLabel').focus();
    }
    function closeEditor() { editor.hidden = true; renderMacros(); mkeys[editingSlot].focus(); }
    editor.addEventListener('submit', function (e) {
      e.preventDefault(); var label = $('#macroLabel').value.trim();
      if (!label) { $('#macroLabel').focus(); return; }
      MACROS[editingSlot] = [label, $('#macroShortcut').value.trim(), MACROS[editingSlot] ? MACROS[editingSlot][2] : null];
      log('out', 'Key ' + (editingSlot + 1) + ' saved · ' + label + ' · ' + (MACROS[editingSlot][1] || 'touch only'));
      closeEditor();
    });
    $('#macroRemove').addEventListener('click', function () { MACROS[editingSlot] = null; log('out', 'Key ' + (editingSlot + 1) + ' cleared'); closeEditor(); });
    $('#macroCancel').addEventListener('click', closeEditor);
    editor.addEventListener('keydown', function (e) { if (e.key === 'Escape') { e.stopPropagation(); closeEditor(); } });
    $('#macroEditToggle').addEventListener('click', function () { editingMacros = !editingMacros; this.setAttribute('aria-pressed', String(editingMacros)); this.textContent = editingMacros ? 'PICK A KEY TO EDIT · DONE' : 'EDIT KEYS'; });
    function pressMacro(i, how) {
      var m = MACROS[i], el = mkeys[i];
      if (!el) return;
      if (!m) { openEditor(i); return; }
      el.classList.add('is-down'); later(function () { el.classList.remove('is-down'); }, 160);
      log('out', 'Preview · ' + m[0] + ' → ' + m[1] + ' (' + how + ')');
      audio.blip(600 + i * 40, 0.05);
    }
    renderMacros();
    function macroFor(binding) {
      for (var i = 0; i < MACROS.length; i++) if (MACROS[i] && MACROS[i][2] === binding) return i;
      return -1;
    }

    /* ---- usage ---- */
    var USAGE = [['Claude Pro · session', 62, 'resets in 4 h'], ['Claude Pro · week', 38, 'resets in 4 days'], ['Codex · OpenAI', 81, 'resets tomorrow']];
    var usageEl = $('#usage');
    function renderUsage(animate) {
      if (!usageEl) return;
      usageEl.innerHTML = '';
      USAGE.forEach(function (u, i) {
        var d = document.createElement('div');
        d.className = 'ubar' + (i === usageHl ? ' is-hl' : '');
        d.innerHTML = '<div class="ubar__lbl"><span>' + u[0] + (i === usageHl ? ' ▸' : '') + '</span><span>' + u[1] + '%</span></div>' +
          '<div class="ubar__track"><div class="ubar__fill' + (u[1] > 75 ? ' ubar__fill--hot' : '') + '" style="width:' + (animate ? 0 : u[1]) + '%"></div></div>' +
          '<div class="ubar__lbl"><span class="dim" style="color:#8f8f9b">' + u[2] + '</span></div>';
        usageEl.appendChild(d);
      });
      if (animate) later(function () { $$('.ubar__fill', usageEl).forEach(function (f, i) { f.style.width = USAGE[i][1] + '%'; }); }, 40);
    }

    /* ---- omarchy ---- */
    var WS = ['1', '2', '3', '4', '5'];
    var WINDOWS = [['Ghostty · Claude', 'Ghostty · Codex'], ['Firefox · documentation', 'Chromium · preview'], ['KiCad · main-revb', 'FreeCAD · enclosure'], ['Editor · firmware', 'Terminal · build'], ['Music · focus mix']];
    var chipsEl = $('#wsChips');
    function renderWs() {
      if (!chipsEl) return;
      chipsEl.replaceChildren();
      WS.forEach(function (w, i) {
        var button = document.createElement('button'); button.type = 'button'; button.className = 'chip' + (i === wsHl ? ' is-on' : ''); button.textContent = 'ws ' + w;
        button.setAttribute('aria-pressed', String(i === wsHl));
        button.addEventListener('click', function () { wsHl = i; renderWs(); log('out', 'Workspace ' + w + ' selected'); audio.blip(540, 0.04); });
        chipsEl.appendChild(button);
      });
      var rows = $('#wsWindows'); rows.replaceChildren();
      WINDOWS[wsHl].forEach(function (name) {
        var li = document.createElement('li'), button = document.createElement('button');
        button.type = 'button'; button.className = 'window-button'; button.textContent = name;
        button.addEventListener('click', function () { $$('.window-button', rows).forEach(function (b) { b.classList.remove('is-focused'); }); button.classList.add('is-focused'); $('#wsStatus').textContent = 'Focused · ' + name; log('out', 'Window focus · ' + name); });
        li.appendChild(button); rows.appendChild(li);
      });
      $('#wsStatus').textContent = 'Workspace ' + WS[wsHl] + ' · ' + WINDOWS[wsHl].length + ' windows';
    }

    /* System metrics are clearly labelled samples, never browser telemetry. */
    var systemRefresh = 0;
    function renderSystem() {
      var rows = [['CPU', 24 + systemRefresh % 12, 'Total processor load'], ['Memory', 58, '18.6 / 32 GiB used'], ['GPU', 12 + systemRefresh % 8, 'Graphics processor load'], ['Storage', 68, '348 / 512 GiB free']];
      $('#systemMeters').innerHTML = rows.map(function (m) { return '<div class="system-meter"><span>' + m[0] + '</span><b>' + m[1] + '%</b><progress max="100" value="' + m[1] + '"></progress><small>' + m[2] + '</small></div>'; }).join('');
    }
    $('#refreshSystem').addEventListener('click', function () { systemRefresh += 3; renderSystem(); log('in', 'Sample metrics refreshed · CPU, memory, GPU and free storage'); });
    renderSystem();

    var arcade = window.OWTArcade({ log: log, blip: audio.blip });

    /* ---- the screen chrome ---- */
    var HINTS = {
      roster:  '<kbd>‹ ›</kbd> switch <kbd>SCROLL</kbd> select <kbd>ENTER</kbd> open <kbd>PTT</kbd> talk',
      channel: '<kbd>‹ ›</kbd> agents <kbd>ENTER</kbd> card <kbd>CANCEL</kbd> roster',
      card:    '<kbd>SCROLL</kbd> choose <kbd>ENTER</kbd> confirm <kbd>PTT</kbd> reply <kbd>CANCEL</kbd> snooze',
      OMARCHY: '<kbd>‹ ›</kbd> workspace <kbd>ENTER</kbd> go <kbd>CANCEL</kbd> home',
      MACROPAD: 'tap or press · <kbd>SHIFT</kbd>+<kbd>CANCEL</kbd> edits the keys',
      USAGE:   '<kbd>SCROLL</kbd> providers <kbd>ENTER</kbd> refresh',
      SYSTEM: '<kbd>ENTER</kbd> refresh sample metrics',
      ARCADE: '<kbd>‹ ›</kbd> move <kbd>PTT</kbd> play <kbd>ESC</kbd> back',
    };
    var TAGS = { AGENTS: '19:42', OMARCHY: 'remote', MACROPAD: 'deck.toml', USAGE: 'sample', SYSTEM: 'sample', ARCADE: 'local' };

    var GUIDES = [
      ['A direct line to your agents.', 'Open a session, answer a question, or hold PTT to send a sample prompt.', 'Try: ENTER twice to open the first question.'],
      ['Your desktop, within reach.', 'Switch workspaces and tap a window to focus your Omarchy desktop.', 'Try: workspace 3, then select KiCad.'],
      ['Make every key yours.', 'Label nine touch keys and assign shortcuts for your everyday actions.', 'Try: EDIT KEYS, choose a key, then save.'],
      ['Know how much is left.', 'See Claude and Codex usage windows, with a countdown to each reset.', 'Try: scroll providers; ENTER refreshes.'],
      ['Your computer, at a glance.', 'Check sample CPU, memory, GPU and free storage readings in one place.', 'Try: ENTER to refresh the sample metrics.'],
      ['A break between builds.', 'Play Flappy Bird, Snake or Raiden, adapted from the deck’s local games.', 'Try: pick a game; PTT plays, ESC goes back.']
    ];
    $$('[data-demo-face]').forEach(function (button) { button.addEventListener('click', function () { setFace(Number(button.dataset.demoFace)); }); });

    function paint() {
      faceEls.forEach(function (f, i) { f.classList.toggle('is-active', i === face); });
      var name = FACES[face];
      $$('[data-demo-face]').forEach(function (b, i) { b.setAttribute('aria-pressed', String(i === face)); });
      $('#demoEyebrow').textContent = '0' + (face + 1) + ' / ' + name;
      $('#demoTitle').textContent = GUIDES[face][0]; $('#demoDescription').textContent = GUIDES[face][1]; $('#demoTip').textContent = GUIDES[face][2];
      if (title) title.textContent = name === 'AGENTS' && view !== 'roster' ? (agents[chan].kind + ' · ' + agents[chan].name).toUpperCase() : name;
      if (tag) { tag.textContent = TAGS[name]; tag.classList.toggle('is-demo', name === 'SYSTEM' || name === 'USAGE'); }
      if (hint) hint.innerHTML = name === 'AGENTS' ? HINTS[view] : HINTS[name];
      $$('[data-view]', deckEl).forEach(function (v) { v.hidden = v.getAttribute('data-view') !== view; });
      renderRoster();
      if (view === 'channel') renderChannel();
      if (view === 'card') renderCard();
    }

    function openChannel() { view = 'channel'; paint(); log('out', 'open · ' + agents[chan].kind + ' · ' + agents[chan].name, 'dim'); audio.blip(620, 0.05); }

    /* ---- the controls ---- */
    function setFace(n) {
      var next = clamp(n, 0, FACES.length - 1);
      if (next === face) { audio.blip(220, 0.04, 'sawtooth'); return; }   // hard end, one small bounce
      if (pttHeld) pttUp();
      arcade.deactivate(); editor.hidden = true;
      face = next; view = 'roster'; sent = false;
      if (FACES[face] === 'USAGE') renderUsage(true);
      paint();
      log('out', 'face ' + (face + 1) + ' · ' + FACES[face].toLowerCase());
      audio.blip(480 + face * 60, 0.05);
    }

    function detent(dir) {
      if (drum) { var cur = parseFloat(drum.style.getPropertyValue('--spin')) || 0; drum.style.setProperty('--spin', (cur + dir * 12) + 'px'); }
      if (shiftHeld) { setFace(face + dir); return; }
      var name = FACES[face];
      if (name === 'AGENTS') {
        if (view === 'roster') { hl = clamp(hl + dir, 0, agents.length - 1); renderRoster(); }
        else if (view === 'card' && !sent) { var n = agents[chan].q ? agents[chan].q.choices.length : agents[chan].ready ? agents[chan].ready.steps.length : 1; choice = clamp(choice + dir, 0, n - 1); renderCard(); }
        else if (view === 'channel') { log('out', 'scroll · reads the long reply', 'dim'); }
      } else if (name === 'ARCADE') { arcade.action(dir < 0 ? 'left' : 'right'); }
      else if (name === 'USAGE') { usageHl = clamp(usageHl + dir, 0, USAGE.length - 1); renderUsage(false); }
      else if (name === 'OMARCHY') { wsHl = clamp(wsHl + dir, 0, WS.length - 1); renderWs(); log('out', 'key cw/ccw act:2', 'dim'); }
      else if (name === 'MACROPAD') { log('out', 'scroll · no key bound to the wheel', 'dim'); }
      audio.blip(dir > 0 ? 400 : 360, 0.03);
    }

    function enter() {
      var name = FACES[face];
      if (name === 'AGENTS') {
        if (view === 'roster') { chan = hl; openChannel(); }
        else if (view === 'channel') {
          var a = agents[chan];
          if (a.q || a.ready) { view = 'card'; choice = a.q ? a.q.rec : 0; sent = false; paint(); audio.blip(680, 0.05); }
          else { log('out', 'enter · nothing pending for ' + a.name, 'dim'); audio.blip(300, 0.04); }
        }
        else if (view === 'card') confirmCard();
      } else if (name === 'ARCADE') { arcade.action('enter'); }
      else if (name === 'SYSTEM') { $('#refreshSystem').click(); }
      else if (name === 'MACROPAD') { var i = macroFor(shiftHeld ? 'shift+enter' : 'enter'); if (i >= 0) pressMacro(i, 'ENTER'); }
      else if (name === 'USAGE') { log('out', 'usage probe · Claude + Codex windows', 'dim'); renderUsage(true); audio.blip(640, 0.05); }
      else if (name === 'OMARCHY') { var win = $('.window-button', $('#wsWindows')); if (win) win.click(); log('out', 'ws go ' + WS[wsHl]); audio.blip(600, 0.05); }
    }

    function confirmCard() {
      var a = agents[chan];
      if (sent) return;
      sent = true; renderCard();
      if (!a.q && !a.ready) { sent = false; return; }
      var label = a.q ? a.q.choices[choice] : a.ready.steps[choice];
      log('out', 'answer { agent: ' + (chan + 1) + ', choice: ' + (choice + 1) + ' } "' + label + '"');
      audio.blip(880, 0.08);
      later(function () {
        log('in', 'card cleared · ' + a.kind + ' · ' + a.name + ' is working', 'host');
        a.state = 'w'; a.q = null; a.ready = null; a.age = 0; a.sub = 'Running "' + label.toLowerCase() + '" · 0s';
        if (face === 0 && chan === agents.indexOf(a)) view = 'channel'; sent = false; paint();
        audio.blip(520, 0.05);
      }, 900);
    }

    function cancel() {
      var name = FACES[face];
      if (!editor.hidden) { closeEditor(); return; }
      if (name === 'ARCADE' && arcade.action('cancel')) return;
      if (name === 'AGENTS' && view === 'card') { view = 'channel'; sent = false; paint(); log('out', 'snooze · notice waits in the drawer', 'dim'); }
      else if (name === 'AGENTS' && view === 'channel') { view = 'roster'; paint(); }
      else if (name === 'MACROPAD' && shiftHeld) { openEditor(0); }
      else if (face !== 0 || view !== 'roster') { setFace(0); log('out', 'home'); }
      audio.blip(340, 0.05);
    }

    function side(dir) {
      var name = FACES[face];
      if (name === 'AGENTS') {
        if (view === 'roster') { hl = clamp(hl + dir, 0, agents.length - 1); chan = hl; renderRoster(); log('out', 'channel ' + (chan + 1) + ' · ' + agents[chan].kind + ' · ' + agents[chan].name); }
        else if (view === 'channel') { chan = clamp(chan + dir, 0, agents.length - 1); paint(); }
        else if (view === 'card') { log('out', 'walk pending cards · hard ends', 'dim'); }
      } else if (name === 'ARCADE') { arcade.action(dir < 0 ? 'left' : 'right'); }
      else if (name === 'MACROPAD') { var i = macroFor((shiftHeld ? 'shift+' : '') + (dir < 0 ? 'left' : 'right')); if (i >= 0) pressMacro(i, dir < 0 ? 'LEFT' : 'RIGHT'); }
      else if (name === 'OMARCHY') { wsHl = clamp(wsHl + dir, 0, WS.length - 1); renderWs(); log('out', 'key ' + (dir < 0 ? 'left' : 'right') + (shiftHeld ? ' shift' : '')); }
      else { log('out', (dir < 0 ? 'left' : 'right') + ' · nothing on this face', 'dim'); }
      audio.blip(dir < 0 ? 440 : 500, 0.04);
    }

    /* ---- PTT ---- */
    var pttEl = $('#pttOverlay'), pttWho = $('#pttWho'), pttText = $('#pttText'), pttWave = $('#pttWave');
    var LINES = ['Refactor the auth module, then run the test suite.', 'Go ahead on staging, and tell me when the migrations finish.', 'Summarise what changed and open the pull request.'];
    var lineIdx = 0, pttT0 = 0, pttTyping = null;
    if (pttWave) for (var w = 0; w < 24; w++) { var bar = document.createElement('i'); bar.style.animationDelay = (w * 40) + 'ms'; bar.style.animationDuration = (380 + (w % 5) * 90) + 'ms'; pttWave.appendChild(bar); }

    function pttDown() {
      if (FACES[face] === 'ARCADE') { arcade.action('ptt'); return; }
      if (pttHeld) return;
      pttHeld = true; pttT0 = Date.now();
      var a = agents[chan];
      if (pttTyping) pttTyping.stop();
      if (pttWho) pttWho.textContent = a.kind + ' · ' + a.name;
      if (pttEl) pttEl.classList.add('is-on');
      if (pttText) pttTyping = typer(pttText, LINES[lineIdx]);
      log('out', 'ptt down · LISTENING · ' + a.kind + ' · ' + a.name);
      audio.blip(520, 0.05);
    }
    function pttUp() {
      if (!pttHeld) return;
      pttHeld = false;
      if (pttTyping) pttTyping.stop();
      var line = LINES[lineIdx]; lineIdx = (lineIdx + 1) % LINES.length;
      var secs = ((Date.now() - pttT0) / 1000).toFixed(1);
      var a = agents[chan];
      log('out', 'ptt up · ' + secs + ' s · "' + line + '"');
      later(function () {
        if (pttHeld) return;
        if (pttEl) pttEl.classList.remove('is-on');
        if (face === 0 && view === 'card' && agents[chan] === a) { log('in', 'answered by voice · card cleared', 'host'); a.q = null; a.ready = null; view = 'channel'; }
        else log('in', 'prompt delivered · ' + a.kind + ' · ' + a.name + ' is working', 'host');
        a.state = 'w'; a.age = 0; a.sub = 'Working on your request · 0s';
        paint();
      }, 500);
      audio.blip(760, 0.06);
    }

    /* ---- wire the physical controls ---- */
    var keyByName = {};
    keys.forEach(function (k) { keyByName[k.getAttribute('data-key')] = k; });

    function flash(k) { if (!k) return; k.classList.add('is-down'); later(function () { k.classList.remove('is-down'); }, 110); }

    function press(name) {
      if (name === 'enter') enter();
      else if (name === 'cancel') cancel();
      else if (name === 'left') side(-1);
      else if (name === 'right') side(1);
      flash(keyByName[name]);
    }

    keys.forEach(function (k) {
      var name = k.getAttribute('data-key');
      if (name === 'ptt') {
        k.addEventListener('pointerdown', function (e) { e.preventDefault(); try { k.setPointerCapture(e.pointerId); } catch (err) {} k.classList.add('is-held'); pttDown(); });
        ['pointerup', 'pointercancel'].forEach(function (ev) { k.addEventListener(ev, function () { k.classList.remove('is-held'); pttUp(); }); });
        k.addEventListener('keydown', function (e) { if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) { e.preventDefault(); k.classList.add('is-held'); pttDown(); } });
        k.addEventListener('keyup', function (e) { if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); k.classList.remove('is-held'); pttUp(); } });
      } else if (name === 'shift') {
        k.addEventListener('click', function () { shiftHeld = !shiftHeld; k.classList.toggle('is-held', shiftHeld); k.setAttribute('aria-pressed', String(shiftHeld)); log('out', shiftHeld ? 'SHIFT held · scroll now moves between faces' : 'SHIFT released', 'dim'); audio.blip(shiftHeld ? 500 : 420, 0.04); });
      } else {
        k.addEventListener('click', function () { press(name); });
      }
    });

    if (wheel) {
      var dragging = false, acc = 0, lastY = 0;
      wheel.addEventListener('pointerdown', function (e) { dragging = true; acc = 0; lastY = e.clientY; try { wheel.setPointerCapture(e.pointerId); } catch (err) {} e.preventDefault(); });
      wheel.addEventListener('pointermove', function (e) {
        if (!dragging) return;
        acc += e.clientY - lastY; lastY = e.clientY;
        while (acc > 18) { acc -= 18; detent(1); }
        while (acc < -18) { acc += 18; detent(-1); }
      });
      ['pointerup', 'pointercancel'].forEach(function (ev) { wheel.addEventListener(ev, function () { dragging = false; }); });
      wheel.addEventListener('wheel', function (e) { e.preventDefault(); detent(e.deltaY > 0 ? 1 : -1); }, { passive: false });
      wheel.addEventListener('keydown', function (e) {
        if (e.key === 'ArrowDown' || e.key === 'ArrowRight') { e.preventDefault(); detent(1); }
        if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') { e.preventDefault(); detent(-1); }
      });
    }

    /* ---- physical keyboard, only while the replica is on screen ---- */
    var MAP = { ArrowDown: 'down', ArrowUp: 'up', Enter: 'enter', Escape: 'cancel', ArrowLeft: 'left', ArrowRight: 'right', ' ': 'ptt', Shift: 'shift' };
    document.addEventListener('keydown', function (e) {
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
      var t = (e.target.tagName || '').toLowerCase();
      if (t === 'input' || t === 'textarea' || t === 'select' || e.target.isContentEditable || t === 'button' && (e.key === 'Enter' || e.key === ' ') || t === 'button' && e.target.classList.contains('pttbtn')) return;
      var fn = MAP[e.key];
      if (!fn) return;
      var r = deckEl.getBoundingClientRect();
      if (r.bottom < 0 || r.top > window.innerHeight) return;
      e.preventDefault();
      if (fn === 'shift') { if (!shiftHeld) { shiftHeld = true; keyByName.shift && keyByName.shift.classList.add('is-held'); if (keyByName.shift) keyByName.shift.setAttribute('aria-pressed', 'true'); } return; }
      if (e.repeat) return;
      if (fn === 'down') detent(1);
      else if (fn === 'up') detent(-1);
      else if (fn === 'ptt') { keyByName.ptt && keyByName.ptt.classList.add('is-held'); pttDown(); }
      else press(fn);
    });
    document.addEventListener('keyup', function (e) {
      if (e.key === 'Shift') { shiftHeld = false; keyByName.shift && keyByName.shift.classList.remove('is-held'); if (keyByName.shift) keyByName.shift.setAttribute('aria-pressed', 'false'); }
      if (e.key === ' ') { keyByName.ptt && keyByName.ptt.classList.remove('is-held'); if (pttHeld) pttUp(); }
    });

    window.addEventListener('blur', function () { if (pttHeld) pttUp(); shiftHeld = false; keys.forEach(function (k) { k.classList.remove('is-held'); }); if (keyByName.shift) keyByName.shift.setAttribute('aria-pressed', 'false'); arcade.deactivate(); });

    /* ---- go ---- */
    renderUsage(false); renderWs();
    paint();
    log('in', 'hello · deckd 0.2.0 · 4 agents · usage · macros', 'host');
    log('in', 'card · Codex · cohdl-api asks a question', 'host');
  })();

  (function faceAtlas() {
    var tabs = $$('.face-atlas__tab');
    function select(index, focus) {
      tabs.forEach(function (tab, i) {
        tab.setAttribute('aria-selected', String(i === index)); tab.tabIndex = i === index ? 0 : -1;
        document.getElementById(tab.getAttribute('aria-controls')).hidden = i !== index;
      });
      if (focus) tabs[index].focus();
    }
    tabs.forEach(function (tab, i) {
      tab.addEventListener('click', function () { select(i, false); });
      tab.addEventListener('keydown', function (e) {
        var next = e.key === 'Home' ? 0 : e.key === 'End' ? tabs.length - 1 : e.key === 'ArrowDown' || e.key === 'ArrowRight' ? (i + 1) % tabs.length : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? (i - 1 + tabs.length) % tabs.length : null;
        if (next !== null) { e.preventDefault(); select(next, true); }
      });
    });
  })();

  /* ------------------------------------------------------------
     8b. PHOTO STACKS — one module, every .shots-col on the page
     ------------------------------------------------------------ */
  (function photoStacks() {
    $$('.shots-col').forEach(function (column) {
      var stack = $('.shots', column);
      if (!stack) return;
      var shots = $$('.shot', stack);
      var prevBtn = $('.shots__prev', column);
      var nextBtn = $('.shots__next', column);
      var count = $('.shots__count', column);
      var total = shots.length;
      var front = 0;

      function paint() {
        shots.forEach(function (shot, i) {
          var pos = (i - front + total) % total;
          shot.setAttribute('data-pos', pos);
          shot.setAttribute('aria-hidden', pos === 0 ? 'false' : 'true');
        });
        if (count) count.textContent = (front + 1) + ' / ' + total;
      }
      function go(step) {
        if (total < 2) return;
        front = (front + step + total) % total;
        paint();
        window.OWT_blip && window.OWT_blip(step > 0 ? 520 : 460, 0.05);
      }

      if (prevBtn) prevBtn.addEventListener('click', function () { go(-1); });
      if (nextBtn) nextBtn.addEventListener('click', function () { go(1); });
      if (total < 2 && prevBtn && nextBtn) { prevBtn.disabled = nextBtn.disabled = true; }

      var startX = null, startY = null, swiped = false;
      stack.addEventListener('pointerdown', function (e) { startX = e.clientX; startY = e.clientY; swiped = false; });
      stack.addEventListener('pointermove', function (e) { if (startX !== null && Math.abs(e.clientX - startX) > 12) swiped = true; });
      stack.addEventListener('pointerup', function (e) {
        if (startX === null) return;
        var dx = e.clientX - startX, dy = e.clientY - startY;
        startX = startY = null;
        if (swiped && Math.abs(dx) > Math.abs(dy)) go(dx < 0 ? 1 : -1);
        else if (!swiped) go(1);
      });
      stack.addEventListener('pointercancel', function () { startX = startY = null; });
      column.addEventListener('keydown', function (e) {
        if (e.key === 'ArrowLeft') { e.preventDefault(); go(-1); }
        else if (e.key === 'ArrowRight') { e.preventDefault(); go(1); }
      });
      paint();
    });
  })();

  /* ------------------------------------------------------------
     8c. THE FILM — PLACEHOLDER
     Put the YouTube id in VIDEO_ID (and keep frame-src in
     _headers) and the tube plays it on the first press. Until
     then the set stays on standby and says so.
     ------------------------------------------------------------ */
  (function film() {
    var button = $('#filmPlay');
    var mount = $('#filmMount');
    if (!button || !mount) return;

    var VIDEO_ID = '';   // PLACEHOLDER: no film yet

    button.addEventListener('click', function () {
      if (!VIDEO_ID) {
        var label = $('.tv__play-label', button);
        if (label) label.innerHTML = '<span class="blink" aria-hidden="true">▮</span> NO TAPE YET';
        window.OWT_blip && window.OWT_blip(220, 0.08, 'sawtooth');
        return;
      }
      var frame = document.createElement('iframe');
      frame.src = 'https://www.youtube-nocookie.com/embed/' + VIDEO_ID + '?autoplay=1&rel=0&modestbranding=1&playsinline=1';
      frame.title = 'OpenWalkieTalkie — the film';
      frame.allow = 'accelerometer; autoplay; encrypted-media; gyroscope; picture-in-picture; web-share';
      frame.referrerPolicy = 'strict-origin-when-cross-origin';
      frame.allowFullscreen = true;
      mount.replaceChildren(frame);
      var screen = mount.closest('.tv__screen');
      if (screen) screen.classList.add('is-playing');
      window.OWT_blip && window.OWT_blip(660, 0.06);
      frame.focus();
    });
  })();

  /* ------------------------------------------------------------
     9. Button press feedback
     ------------------------------------------------------------ */
  (function buttons() {
    $$('.btn').forEach(function (b) {
      b.addEventListener('click', function (e) {
        if (b.classList.contains('btn--soon')) e.preventDefault();
        window.OWT_blip && window.OWT_blip(700, 0.06);
      });
    });
    $$('.nav__links a, .footer__links a, .scroll-hint').forEach(function (a) {
      a.addEventListener('click', function () {
        window.OWT_blip && window.OWT_blip(600, 0.045);
      });
    });
  })();

})();
