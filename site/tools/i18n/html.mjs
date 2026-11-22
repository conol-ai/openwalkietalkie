// A small HTML parser for the site's own, well-formed pages. It keeps source
// offsets so callers can cut text out of a page and put it back byte for byte.
// It is not a general HTML5 parser and does not need to be.

export const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);
const RAW = new Set(['script', 'style', 'textarea', 'title']);
// Elements whose end tag may be omitted; an open one is closed by a sibling of these.
const AUTO_CLOSE = { p: /^(p|div|ul|ol|table|h[1-6]|pre|section|header|footer|figure|dl|blockquote|nav|form|details)$/, li: /^li$/, dt: /^(dt|dd)$/, dd: /^(dt|dd)$/, option: /^option$/ };

/** Parse `src` into a tree of { type, name, attrs, start, end, innerStart, innerEnd, children, parent }. */
export function parse(src) {
  const root = { type: 'root', name: '#root', attrs: [], start: 0, end: src.length, innerStart: 0, innerEnd: src.length, children: [], parent: null };
  let cur = root;
  let i = 0;
  const text = (from, to) => { if (to > from) cur.children.push({ type: 'text', start: from, end: to, parent: cur }); };
  while (i < src.length) {
    const lt = src.indexOf('<', i);
    if (lt < 0) { text(i, src.length); break; }
    text(i, lt);
    if (src.startsWith('<!--', lt)) {
      const end = src.indexOf('-->', lt + 4);
      if (end < 0) throw new Error(`unclosed comment at ${lt}`);
      cur.children.push({ type: 'comment', start: lt, end: end + 3, parent: cur });
      i = end + 3; continue;
    }
    if (src[lt + 1] === '!' || src[lt + 1] === '?') {
      const end = src.indexOf('>', lt);
      cur.children.push({ type: 'doctype', start: lt, end: end + 1, parent: cur });
      i = end + 1; continue;
    }
    if (src[lt + 1] === '/') {
      const end = src.indexOf('>', lt);
      const name = src.slice(lt + 2, end).trim().toLowerCase();
      let n = cur;
      while (n && n.name !== name) n = n.parent;
      if (!n || n.type === 'root') throw new Error(`stray </${name}> at ${lt} (line ${src.slice(0, lt).split('\n').length})`);
      while (cur !== n) { cur.end = lt; cur.innerEnd = lt; cur = cur.parent; }
      n.innerEnd = lt; n.end = end + 1; cur = n.parent;
      i = end + 1; continue;
    }
    if (!/[a-zA-Z]/.test(src[lt + 1] ?? '')) { text(lt, lt + 1); i = lt + 1; continue; }
    const tag = readTag(src, lt);
    const name = tag.name;
    while (AUTO_CLOSE[cur.name] && AUTO_CLOSE[cur.name].test(name)) { cur.end = lt; cur.innerEnd = lt; cur = cur.parent; }
    const node = { type: 'element', name, attrs: tag.attrs, start: lt, end: tag.end, innerStart: tag.end, innerEnd: tag.end, children: [], parent: cur };
    cur.children.push(node);
    i = tag.end;
    if (VOID.has(name) || tag.selfClosing) continue;
    if (RAW.has(name)) {
      const close = src.toLowerCase().indexOf(`</${name}`, i);
      if (close < 0) throw new Error(`unclosed <${name}>`);
      if (close > i) node.children.push({ type: 'text', start: i, end: close, parent: node, raw: true });
      node.innerEnd = close; node.end = src.indexOf('>', close) + 1; i = node.end; continue;
    }
    cur = node;
  }
  if (cur !== root) {
    // Unclosed optional elements at end of input.
    while (cur !== root) { cur.end = src.length; cur.innerEnd = src.length; cur = cur.parent; }
  }
  return root;
}

function readTag(src, lt) {
  let i = lt + 1;
  let m = /^[a-zA-Z][a-zA-Z0-9:-]*/.exec(src.slice(i, i + 64));
  const name = m[0].toLowerCase();
  i += m[0].length;
  const attrs = [];
  for (;;) {
    while (/\s/.test(src[i])) i++;
    if (src[i] === '>') return { name, attrs, end: i + 1, selfClosing: false };
    if (src.startsWith('/>', i)) return { name, attrs, end: i + 2, selfClosing: true };
    m = /^[^\s=>\/"']+/.exec(src.slice(i, i + 128));
    if (!m) throw new Error(`bad attribute in <${name}> at ${i}`);
    const attr = { name: m[0].toLowerCase(), value: null, valueStart: -1, valueEnd: -1 };
    i += m[0].length;
    while (/\s/.test(src[i])) i++;
    if (src[i] === '=') {
      i++;
      while (/\s/.test(src[i])) i++;
      const q = src[i];
      if (q === '"' || q === "'") {
        const close = src.indexOf(q, i + 1);
        attr.valueStart = i + 1; attr.valueEnd = close; i = close + 1;
      } else {
        const v = /^[^\s>]+/.exec(src.slice(i));
        attr.valueStart = i; attr.valueEnd = i + v[0].length; i += v[0].length;
      }
      attr.value = src.slice(attr.valueStart, attr.valueEnd);
    }
    attrs.push(attr);
  }
}

export const attr = (node, name) => node.attrs?.find(a => a.name === name)?.value ?? null;

export function* walk(node) {
  yield node;
  for (const child of node.children ?? []) yield* walk(child);
}

/** Every attribute in the document as { node, attr }, in source order. */
export function attributes(root) {
  const out = [];
  for (const node of walk(root)) if (node.type === 'element') for (const a of node.attrs) out.push({ node, attr: a });
  return out;
}
