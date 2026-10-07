'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

// A token scan locates keys without interpreting/reformatting unrelated values.
// Strings, comments and multiline arrays cannot masquerade as a profiles table.
function tokens(text) {
  const result = [];
  for (let i = 0; i < text.length;) {
    const start = i, c = text[i];
    if (c === '\n') { result.push({value:'\n',start,end:++i}); continue; }
    if (/\s/.test(c)) { i++; continue; }
    if (c === '#') { while (i < text.length && text[i] !== '\n') i++; continue; }
    if (c === '"' || c === "'") {
      const triple = text.slice(i, i+3) === c.repeat(3);
      const length = triple ? 3 : 1;
      i += length;
      const body = i;
      let closed = false;
      while (i < text.length) {
        if (c === '"' && text[i] === '\\') { i += 2; continue; }
        if (text.slice(i, i+length) === c.repeat(length)) {
          if (triple) {
            let run = length;
            while (text[i+run] === c) run++;
            if (run > 5) throw new Error('Invalid multiline string delimiter');
            i += run-length;
          }
          closed = true; break;
        }
        i++;
      }
      if (!closed) throw new Error('Unterminated string in store configuration');
      const raw = text.slice(body, i);
      let value = raw;
      if (c === '"' && !triple) {
        try {
          value = JSON.parse('"' + raw.replace(/\\U([0-9a-fA-F]{8})/g,
            (_, hex) => String.fromCodePoint(parseInt(hex,16))) + '"');
        } catch (_) { value = raw; } // Values are opaque; these escapes are not profile keys.
      }
      i += length;
      result.push({value,start,end:i,quoted:true});
      continue;
    }
    if ('.=[]{},'.includes(c)) { result.push({value:c,start,end:++i}); continue; }
    while (i < text.length && !/[\s.#=\[\]{},'"]/.test(text[i])) i++;
    if (i === start) throw new Error('Unsupported token in store configuration');
    result.push({value:text.slice(start,i),start,end:i});
  }
  return result;
}
function key(parts) {
  if (!parts.length || parts.length % 2 !== 1) return null;
  if (parts.some((t,i) => i%2 ? t.value !== '.' : !t.quoted && !/^[A-Za-z0-9_-]+$/.test(t.value))) return null;
  return parts.filter((_,i) => i%2 === 0).map(t => t.value);
}
function addLastDefault(text) {
  const ts = tokens(text);
  let section = [], profileHeader = null, firstHeader = text.length;
  let dotted = false, inline = null;
  for (let at = 0; at < ts.length;) {
    if (ts[at].value === '\n') { at++; continue; }
    const start = at;
    let depth = 0;
    while (at < ts.length) {
      const v = ts[at].value;
      if (v === '\n' && depth === 0) break;
      if (!ts[at].quoted && (v === '[' || v === '{')) depth++;
      if (!ts[at].quoted && (v === ']' || v === '}')) depth--;
      at++;
    }
    const statement = ts.slice(start, at).filter(t => t.value !== '\n');
    if (statement[0]?.value === '[') {
      const array = statement[1]?.value === '[';
      const trim = array ? 2 : 1;
      section = key(statement.slice(trim, -trim)) || [];
      firstHeader = Math.min(firstHeader, statement[0].start);
      if (!array && section.length === 1 && section[0] === 'profiles') {
        // Insert after the complete header/comment line.
        profileHeader = at < ts.length ? ts[at].end : text.length;
      }
      continue;
    }
    const equal = statement.findIndex(t => t.value === '=');
    if (equal < 0) continue;
    const name = key(statement.slice(0,equal));
    if (!name) continue;
    if (section.length === 1 && section[0] === 'profiles' && name[0] === 'opt_in') return text;
    if (section.length === 0 && name[0] === 'profiles') {
      if (name[1] === 'opt_in') return text;
      if (name.length > 1) { dotted = true; continue; }
      const value = statement.slice(equal+1);
      if (value[0]?.value !== '{' || value.at(-1)?.value !== '}') {
        throw new Error('profiles must be a TOML table');
      }
      let nested = 0, fieldStart = 1;
      for (let j=1;j<value.length-1;j++) {
        const v = value[j].value;
        if (nested === 0 && v === '=') {
          const field = key(value.slice(fieldStart,j));
          if (field?.[0] === 'opt_in') return text;
        }
        if (!value[j].quoted && (v === '[' || v === '{')) nested++;
        if (!value[j].quoted && (v === ']' || v === '}')) nested--;
        if (!value[j].quoted && nested === 0 && v === ',') fieldStart = j+1;
      }
      const before = value[value.length-2];
      inline = {at:value.at(-1).start, comma:before.value !== '{' && before.value !== ','};
    }
  }
  if (inline) return text.slice(0,inline.at) + (inline.comma ? ', ' : '') + 'opt_in = ["last"] ' + text.slice(inline.at);
  if (profileHeader !== null) {
    const separator = profileHeader && text[profileHeader-1] !== '\n' ? '\n' : '';
    return text.slice(0,profileHeader) + separator + 'opt_in = ["last"]\n' + text.slice(profileHeader);
  }
  if (dotted) return text.slice(0,firstHeader) + (firstHeader && text[firstHeader-1] !== '\n' ? '\n' : '') + 'profiles.opt_in = ["last"]\n' + text.slice(firstHeader);
  return text + (text && !text.endsWith('\n') ? '\n' : '') + '\n[profiles]\nopt_in = ["last"]\n';
}
function ensureLastDefault(storeDir) {
  fs.mkdirSync(storeDir, {recursive:true});
  const target = path.join(storeDir,'ti.toml');
  if (fs.existsSync(target) && fs.statSync(target).size > 1024*1024) throw new Error('Store configuration exceeds 1 MiB');
  const before = fs.existsSync(target) ? fs.readFileSync(target,'utf8') : '';
  const after = addLastDefault(before);
  if (before === after) return false;
  const temporary = target + '.' + crypto.randomBytes(8).toString('hex') + '.new';
  try {
    const fd = fs.openSync(temporary,'wx',0o600);
    try { fs.fchmodSync(fd,0o600); fs.writeFileSync(fd,after); }
    finally { fs.closeSync(fd); }
    fs.renameSync(temporary,target);
  } finally { fs.rmSync(temporary,{force:true}); }
  return true;
}
module.exports = {addLastDefault, ensureLastDefault};
