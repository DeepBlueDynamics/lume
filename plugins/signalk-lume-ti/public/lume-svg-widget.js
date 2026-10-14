'use strict';
// LumeSvgWidget: a custom widget drawn from an SVG fragment with live bindings.
// Model-written SVG is reduced to a whitelist of shapes and attributes, and
// {{expressions}} are evaluated by a small built-in evaluator, never as JavaScript.
(function (root) {
  const ELEMENTS = new Set(['g', 'circle', 'ellipse', 'rect', 'line', 'path', 'polygon', 'polyline', 'text', 'tspan',
    'defs', 'linearGradient', 'radialGradient', 'stop', 'clipPath']);
  const ATTRIBUTES = new Set(['x', 'y', 'x1', 'y1', 'x2', 'y2', 'cx', 'cy', 'r', 'rx', 'ry', 'fx', 'fy', 'width', 'height',
    'd', 'points', 'transform', 'fill', 'stroke', 'stroke-width', 'stroke-linecap', 'stroke-linejoin', 'stroke-dasharray',
    'stroke-dashoffset', 'opacity', 'fill-opacity', 'stroke-opacity', 'font-size', 'font-weight', 'font-family',
    'text-anchor', 'dominant-baseline', 'letter-spacing', 'offset', 'stop-color', 'stop-opacity', 'gradientUnits',
    'gradientTransform', 'id', 'clip-path', 'visibility', 'display']);
  const MAX_SVG = 12000;

  // ---- Expressions: numbers, strings, names, + - * / %, comparisons, && || !, ?:, f(...)
  const FUNCTIONS = {
    deg: r => r * 180 / Math.PI, rad: d => d * Math.PI / 180, round: (v, n = 0) => +Number(v).toFixed(n),
    fixed: (v, n = 1) => (Number.isFinite(v) ? Number(v).toFixed(n) : '—'), abs: Math.abs, min: Math.min, max: Math.max,
    clamp: (v, lo, hi) => Math.min(hi, Math.max(lo, v)), floor: Math.floor, ceil: Math.ceil, sqrt: Math.sqrt,
    sin: Math.sin, cos: Math.cos, mod: (a, b) => ((a % b) + b) % b, knots: ms => ms * 1.94384, celsius: k => k - 273.15,
    pct: r => r * 100, has: v => v !== undefined && v !== null && !(typeof v === 'number' && Number.isNaN(v)),
  };

  function tokenize(src) {
    const out = [];
    let i = 0;
    while (i < src.length) {
      const c = src[i];
      if (/\s/.test(c)) { i++; continue; }
      const num = /^(\d+\.?\d*|\.\d+)(e[+-]?\d+)?/i.exec(src.slice(i));
      if (num) { out.push({t: 'num', v: parseFloat(num[0])}); i += num[0].length; continue; }
      if (c === "'" || c === '"') {
        const end = src.indexOf(c, i + 1);
        if (end < 0) throw new Error('unclosed string');
        out.push({t: 'str', v: src.slice(i + 1, end)}); i = end + 1; continue;
      }
      const id = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i));
      if (id) { out.push({t: 'id', v: id[0]}); i += id[0].length; continue; }
      const op = /^(>=|<=|==|!=|&&|\|\||[-+*/%()?:,<>!])/.exec(src.slice(i));
      if (op) { out.push({t: 'op', v: op[0]}); i += op[0].length; continue; }
      throw new Error(`unexpected "${c}"`);
    }
    return out;
  }

  function compile(src) {
    if (src.length > 300) throw new Error('expression too long');
    const tokens = tokenize(src);
    let p = 0;
    const peek = v => tokens[p] && tokens[p].t === 'op' && tokens[p].v === v;
    const take = v => { if (!peek(v)) throw new Error(`expected "${v}"`); p++; };
    const ternary = () => {
      const cond = or();
      if (!peek('?')) return cond;
      p++; const a = ternary(); take(':'); const b = ternary();
      return env => (cond(env) ? a(env) : b(env));
    };
    const binary = (next, ops) => () => {
      let left = next();
      while (tokens[p] && tokens[p].t === 'op' && ops.includes(tokens[p].v)) {
        const op = tokens[p++].v;
        const l = left;
        const r = next();
        left = env => {
          const a = l(env); const b = r(env);
          switch (op) {
            case '||': return a || b; case '&&': return a && b;
            case '==': return a === b; case '!=': return a !== b;
            case '>': return a > b; case '<': return a < b; case '>=': return a >= b; case '<=': return a <= b;
            case '+': return typeof a === 'string' || typeof b === 'string' ? `${a}${b}` : a + b;
            case '-': return a - b; case '*': return a * b; case '/': return a / b; case '%': return a % b;
          }
          return undefined;
        };
      }
      return left;
    };
    const unary = () => {
      if (peek('-')) { p++; const v = unary(); return env => -v(env); }
      if (peek('!')) { p++; const v = unary(); return env => !v(env); }
      return primary();
    };
    const mul = binary(unary, ['*', '/', '%']);
    const add = binary(mul, ['+', '-']);
    const cmp = binary(add, ['>', '<', '>=', '<=']);
    const eq = binary(cmp, ['==', '!=']);
    const and = binary(eq, ['&&']);
    const or = binary(and, ['||']);
    function primary() {
      const tok = tokens[p++];
      if (!tok) throw new Error('unexpected end');
      if (tok.t === 'num' || tok.t === 'str') return () => tok.v;
      if (tok.t === 'op' && tok.v === '(') { const v = ternary(); take(')'); return v; }
      if (tok.t === 'id') {
        if (tok.v === 'true') return () => true;
        if (tok.v === 'false') return () => false;
        if (peek('(')) {
          const fn = FUNCTIONS[tok.v];
          if (!fn) throw new Error(`unknown function ${tok.v}`);
          p++;
          const args = [];
          if (!peek(')')) { do { args.push(ternary()); } while (peek(',') && ++p); }
          take(')');
          return env => fn(...args.map(a => a(env)));
        }
        const name = tok.v;
        return env => (Object.prototype.hasOwnProperty.call(env, name) ? env[name] : undefined);
      }
      throw new Error(`unexpected "${tok.v}"`);
    }
    const expr = ternary();
    if (p !== tokens.length) throw new Error('unexpected text after expression');
    return env => { try { return expr(env); } catch (_) { return undefined; } };
  }

  // A template string with {{...}} parts becomes a function of the values.
  function template(text) {
    const parts = [];
    let last = 0;
    const re = /\{\{([^{}]{1,300})\}\}/g;
    let m;
    while ((m = re.exec(text))) {
      parts.push(text.slice(last, m.index));
      parts.push(compile(m[1].trim()));
      last = re.lastIndex;
    }
    if (!parts.length) return null;
    parts.push(text.slice(last));
    return env => parts.map(part => {
      if (typeof part === 'string') return part;
      const v = part(env);
      if (typeof v === 'number') return Number.isFinite(v) ? String(+v.toFixed(4)) : '';
      return v === undefined || v === null ? '' : String(v);
    }).join('');
  }

  const LOCAL_REF = /^url\(#[A-Za-z0-9_-]{1,40}\)$/;
  // A value is safe if it carries no script, markup or external reference; a local
  // url(#id) is allowed only where it names a gradient or clip path.
  const SAFE_VALUE = (v, name) => {
    const text = String(v);
    if (LOCAL_REF.test(text.trim())) return name === 'fill' || name === 'stroke' || name === 'clip-path';
    return !/javascript:|url\s*\(|expression\s*\(|<|>/i.test(text);
  };

  /** Parse and whitelist an SVG fragment; throws with a reason if it can't be used. */
  function sanitize(fragment, viewBox) {
    if (typeof fragment !== 'string' || !fragment.trim()) throw new Error('svg is empty');
    if (fragment.length > MAX_SVG) throw new Error(`svg is over ${MAX_SVG} characters`);
    const vb = /^-?\d+(\.\d+)? -?\d+(\.\d+)? \d+(\.\d+)? \d+(\.\d+)?$/.test(String(viewBox || '')) ? viewBox : '0 0 200 200';
    const doc = new DOMParser().parseFromString(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="${vb}">${fragment}</svg>`, 'image/svg+xml');
    if (doc.getElementsByTagName('parsererror').length) throw new Error('svg is not well-formed');
    const svgEl = doc.documentElement;
    const bindings = [];
    const walk = node => {
      for (const child of [...node.childNodes]) {
        if (child.nodeType === 3) {
          const fn = template(child.nodeValue);
          if (fn) bindings.push({node: child, fn, kind: 'text'});
          continue;
        }
        if (child.nodeType !== 1 || !ELEMENTS.has(child.localName)) { child.remove(); continue; }
        for (const attr of [...child.attributes]) {
          const ok = ATTRIBUTES.has(attr.name) && SAFE_VALUE(attr.value, attr.name);
          if (!ok) { child.removeAttribute(attr.name); continue; }
          const fn = template(attr.value);
          if (fn) bindings.push({node: child, attr: attr.name, fn, kind: 'attr'});
        }
        walk(child);
      }
    };
    walk(svgEl);
    svgEl.setAttribute('class', 'lume-gauge lume-svg');
    svgEl.setAttribute('role', 'img');
    return {svg: svgEl, bindings};
  }

  /** Render into el; returns {update(values)} where values is {name: value}. */
  function render(el, options = {}) {
    let built;
    try {
      built = sanitize(options.svg, options.viewBox);
    } catch (err) {
      el.textContent = `This widget can't be drawn: ${err.message}`;
      return {update() {}};
    }
    el.appendChild(document.importNode(built.svg, true));
    const live = el.lastElementChild;
    // importNode copies the tree; find the copied nodes by walking both trees in step.
    const map = new Map();
    const pair = (a, b) => { map.set(a, b); for (let i = 0; i < a.childNodes.length; i++) pair(a.childNodes[i], b.childNodes[i]); };
    pair(built.svg, live);
    const bindings = built.bindings.map(b => ({...b, node: map.get(b.node)}));
    const env = {};
    function update(values) {
      Object.assign(env, values);
      for (const b of bindings) {
        const v = b.fn(env);
        if (b.kind === 'text') b.node.nodeValue = v;
        else if (SAFE_VALUE(v, b.attr)) b.node.setAttribute(b.attr, v);
      }
    }
    update(options.initial || {});
    return {update};
  }

  const api = {render, sanitize, compile, template};
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.LumeSvgWidget = api;
})(typeof window !== 'undefined' ? window : globalThis);
