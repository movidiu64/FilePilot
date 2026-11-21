'use strict';
// FilePilot window. Edits the .orgrules document (the same JSON as the file on disk), reads
// the journal and asks for restores. It doesn't run rules: the daemon does.

const TAURI = window.__TAURI__;
const invoke = (cmd, args) => TAURI.core.invoke(cmd, args);
const jsonCmd = async (cmd, args) => JSON.parse(await invoke(cmd, args));

// ---------- temă ----------
let THEME = 'auto';
function setTheme(v) {
  THEME = ['light', 'dark'].includes(v) ? v : 'auto';
  if (THEME === 'auto') delete document.documentElement.dataset.theme; else document.documentElement.dataset.theme = THEME;
}
try { setTheme(localStorage.getItem('theme')); } catch (_) { setTheme('auto'); }

// ---------- stare ----------
const S = {
  doc: { app: 'filepilot', schema_version: 1, rules: [], dictionaries: {} },
  sel: 0,
  dirty: false,
  tab: 'home',
  // Pagina Acasă: cifrele săptămânii și ultimele acțiuni.
  home: null,
  status: null,
  journal: null,
  journalError: null,
  // Confirmările cerute de „Cere confirmare”.
  // Confirmările: totalul, toate id-urile, pagina afișată.
  pend: { total: 0, ids: [], items: [], offset: 0, page: 0, sort: { key: 'ts', desc: false } },
  // Regulile ascunse: există parolă, e deblocat, câte sunt.
  gate: { password: false, unlocked: true, hidden: 0 },
  // Filtrele jurnalului; `limit` crește cu „Arată mai multe” și rămâne la reîmprospătare.
  jq: { text: '', rule: '', status: '', period: '' },
  jlimit: 100,
  pv: { token: 0, loading: false, data: null, error: null, filter: 'all', prog: null, sort: null },
  dictSel: null,
  // The variable preview: whose it is and the text pasted or loaded.
  varPv: { name: null, text: '' },
  autostart: false,
  jsonEdited: false,
};
const sizeUnit = new WeakMap(); // nod de condiție -> unitatea aleasă (doar pentru afișare)
let pvTimer = null;

// ---------- DOM ----------
function h(tag, props = {}, ...kids) {
  const el = document.createElement(tag);
  let deferredValue;
  for (const [k, v] of Object.entries(props || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else if (k === 'value' && tag === 'select') deferredValue = v;
    else if (k in el && k !== 'list') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  if (deferredValue !== undefined) el.value = deferredValue;
  return el;
}

function select(options, value, onchange, label) {
  return h('select', { class: 'sel', 'aria-label': label, value, onchange: (e) => onchange(e.target.value) },
    options.map(([v, text]) => h('option', { value: v }, text)));
}

function textInput(value, oninput, label, extra = {}) {
  return h('input', { class: 'inp', type: 'text', value: value ?? '', 'aria-label': label,
    spellcheck: false, oninput: (e) => oninput(e.target.value), ...extra });
}

function check(label, checked, onchange) {
  return h('label', { class: 'row chk' },
    h('input', { type: 'checkbox', checked: !!checked, onchange: (e) => onchange(e.target.checked) }), label);
}

function icon(name) {
  const paths = {
    x: 'M6 6l12 12M18 6L6 18',
    plus: 'M12 5v14M5 12h14',
    folder: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
    undo: 'M9 14L4 9l5-5M4 9h10a6 6 0 0 1 0 12h-3',
    trash: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
    refresh: 'M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7',
    up: 'M12 19V5M5 12l7-7 7 7',
    down: 'M12 5v14M5 12l7 7 7-7',
    file: 'M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9zM14 3v6h6',
    lock: 'M6 11h12v10H6zM8 11V7a4 4 0 0 1 8 0v4',
    unlock: 'M6 11h12v10H6zM8 11V7a4 4 0 0 1 7.6-1.8',
    eyeOff: 'M3 3l18 18M10.6 6.1A9 9 0 0 1 12 6c5 0 9 6 9 6a15 15 0 0 1-2.4 3M6.6 6.6C4.2 8.1 3 12 3 12s4 6 9 6a8.6 8.6 0 0 0 4.4-1.2M9.9 9.9a3 3 0 0 0 4.2 4.2',
  };
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('width', '16'); svg.setAttribute('height', '16'); svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('fill', 'none'); svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '2');
  svg.setAttribute('stroke-linecap', 'round'); svg.setAttribute('stroke-linejoin', 'round'); svg.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  p.setAttribute('d', paths[name]);
  svg.append(p);
  return svg;
}

function fill(el, ...kids) { el.replaceChildren(...kids.flat(Infinity).filter((k) => k !== null && k !== undefined && k !== false)); }

function windowControls() {
  const w = TAURI && TAURI.window && TAURI.window.getCurrentWindow ? TAURI.window.getCurrentWindow() : null;
  const svg = (d) => {
    const el = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    el.setAttribute('width', '10'); el.setAttribute('height', '10'); el.setAttribute('viewBox', '0 0 10 10');
    el.setAttribute('fill', 'none'); el.setAttribute('stroke', 'currentColor'); el.setAttribute('stroke-width', '1');
    el.setAttribute('aria-hidden', 'true');
    const p = document.createElementNS('http://www.w3.org/2000/svg', 'path'); p.setAttribute('d', d); el.append(p); return el;
  };
  const btn = (cls, label, d, fn) => h('button', { type: 'button', class: 'wbtn ' + cls, title: label, 'aria-label': label,
    onclick: () => { if (w) w[fn]().catch(() => {}); } }, svg(d));
  return h('div', { class: 'wctl' },
    btn('', t('minimize'), 'M0 5h10', 'minimize'),
    btn('', t('maximize'), 'M.5.5h9v9h-9z', 'toggleMaximize'),
    btn('close', t('close'), 'M0 0l10 10M10 0L0 10', 'close'));
}

function iconBtn(name, label, onclick, disabled) {
  return h('button', { type: 'button', class: 'ibtn', title: label, 'aria-label': label, onclick, disabled: !!disabled }, icon(name));
}

/// Mută elementul `i` din `arr` cu `delta` poziții; întoarce noul index.
function moveItem(arr, i, delta) {
  const j = i + delta;
  if (j < 0 || j >= arr.length) return i;
  [arr[i], arr[j]] = [arr[j], arr[i]];
  return j;
}

function toast(msg, isError) {
  const el = document.getElementById('toast');
  el.textContent = msg;
  el.className = 'show' + (isError ? ' err' : '');
  clearTimeout(toast.t);
  toast.t = setTimeout(() => { el.className = ''; }, isError ? 8000 : 3500);
}

/// Întrebare cu mai multe răspunsuri: `choices` = [[valoare, text, clasă], ...]. Esc / clic
/// în afară = 'cancel'.
function choiceModal(message, choices) {
  return new Promise((resolve) => {
    const root = document.getElementById('modal-root');
    const done = (v) => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); done('cancel'); } };
    window.addEventListener('keydown', onKey, true);
    fill(root, h('div', { class: 'overlay', onclick: (e) => { if (e.target === e.currentTarget) done('cancel'); } },
      h('div', { class: 'modal', role: 'dialog', 'aria-modal': 'true' },
        h('p', {}, message),
        h('div', { class: 'row end wrap' }, choices.map(([v, text, cls]) =>
          h('button', { type: 'button', class: 'btn ' + (cls || ''), onclick: () => done(v) }, text))))));
    const first = root.querySelector('.btn.primary');
    if (first) first.focus();
  });
}

/// Fereastră cu câmpuri de parolă. `submit(valori)` întoarce un mesaj de eroare (rămâne
/// deschisă) sau nimic (se închide cu `true`).
function passwordModal(title, fields, okLabel, submit) {
  return new Promise((resolve) => {
    const root = document.getElementById('modal-root');
    const done = (v) => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); done(false); } };
    window.addEventListener('keydown', onKey, true);
    const inputs = fields.map(([, label]) => h('input', { class: 'inp', type: 'password', autocomplete: 'off', 'aria-label': label }));
    const err = h('p', { class: 'field-err', role: 'alert', hidden: true });
    const ok = h('button', { type: 'submit', class: 'btn primary' }, okLabel);
    const go = async (e) => {
      e.preventDefault();
      ok.disabled = true;
      const vals = Object.fromEntries(fields.map(([k], i) => [k, inputs[i].value]));
      let msg = null;
      try { msg = await submit(vals); } catch (x) { msg = String(x); }
      ok.disabled = false;
      if (msg) { err.textContent = msg; err.hidden = false; inputs[0].select(); return; }
      done(true);
    };
    fill(root, h('div', { class: 'overlay', onclick: (e) => { if (e.target === e.currentTarget) done(false); } },
      h('form', { class: 'modal', role: 'dialog', 'aria-modal': 'true', onsubmit: go },
        h('strong', {}, title),
        fields.map(([, label], i) => h('label', { class: 'lbl' }, label, inputs[i])),
        err,
        h('div', { class: 'row end' },
          h('button', { type: 'button', class: 'btn', onclick: () => done(false) }, t('cancel')), ok))));
    inputs[0].focus();
  });
}

function confirmModal(message, okLabel) {
  return new Promise((resolve) => {
    const root = document.getElementById('modal-root');
    const done = (v) => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); done(false); } };
    window.addEventListener('keydown', onKey, true);
    fill(root, h('div', { class: 'overlay', onclick: (e) => { if (e.target === e.currentTarget) done(false); } },
      h('div', { class: 'modal', role: 'dialog', 'aria-modal': 'true' },
        h('p', {}, message),
        h('div', { class: 'row end' },
          h('button', { type: 'button', class: 'btn', onclick: () => done(false) }, t('cancel')),
          h('button', { type: 'button', class: 'btn primary', onclick: () => done(true) }, okLabel || t('ok'))))));
    root.querySelector('.btn.primary').focus();
  });
}

// ---------- utilitare ----------
function fmtBytes(n) {
  if (n < 1024) return n + ' B';
  const u = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return v.toFixed(v < 10 ? 1 : 0) + ' ' + u[i];
}
function fmtTime(ts) {
  return new Date(ts * 1000).toLocaleString(LANG === 'ro' ? 'ro-RO' : 'en-GB', { dateStyle: 'short', timeStyle: 'medium' });
}
function dateToInput(ts) {
  const d = new Date(ts * 1000);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}
function inputToDate(v) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(v);
  return m ? Math.floor(new Date(+m[1], +m[2] - 1, +m[3]).getTime() / 1000) : 0;
}
const UNITS = [['B', 1], ['KB', 1024], ['MB', 1024 ** 2], ['GB', 1024 ** 3]];
function pickUnit(bytes) {
  for (let i = UNITS.length - 1; i > 0; i--) if (bytes >= UNITS[i][1] && bytes % UNITS[i][1] === 0) return UNITS[i][0];
  return 'B';
}
function unitFactor(name) { return UNITS.find((u) => u[0] === name)[1]; }

const rule = () => S.doc.rules[S.sel];
const clone = (x) => JSON.parse(JSON.stringify(x));

function markDirty() {
  S.dirty = true;
  refreshSaveState();
  schedulePreview();
  syncJson();
  syncDictJson();
  refreshIssues();
  if (S.tab === 'rules') refreshSentence();
}

// ---------- editor JSON al regulii ----------
const ruleJson = (r) => JSON.stringify(r, null, 2);

function syncJson() {
  const ta = document.getElementById('json-ta');
  const r = S.doc.rules[S.sel];
  if (ta && r && !S.jsonEdited) ta.value = ruleJson(r);
}

function jsonError(msg) {
  const el = document.getElementById('json-err');
  if (el) el.textContent = msg || '';
}

function applyJson() {
  const ta = document.getElementById('json-ta');
  let obj;
  try { obj = JSON.parse(ta.value); } catch (e) { jsonError(t('jsonInvalid', { msg: e.message })); return; }
  if (obj && Array.isArray(obj.rules)) {
    if (obj.rules.length !== 1) { jsonError(t('jsonMany')); return; }
    obj = obj.rules[0];
  }
  const ok = obj && typeof obj === 'object' && !Array.isArray(obj) && typeof obj.name === 'string'
    && Array.isArray(obj.folders) && Array.isArray(obj.actions) && obj.condition && typeof obj.condition === 'object';
  if (!ok) { jsonError(t('jsonShape')); return; }
  if (S.doc.rules.some((x, i) => i !== S.sel && x.name === obj.name)) { jsonError(t('jsonDupName', { name: obj.name })); return; }
  S.doc.rules[S.sel] = obj;
  S.jsonEdited = false;
  markDirty(); render();
  toast(t('jsonApplied'));
}

async function copyJson() {
  const ta = document.getElementById('json-ta');
  try { await navigator.clipboard.writeText(ta.value); }
  catch (_) { ta.select(); document.execCommand('copy'); }
  toast(t('copied'));
}

async function exportRule() {
  const r = rule();
  try {
    const safe = (r.name || 'regula').replace(/[\\/:*?"<>|]+/g, '_');
    const path = await TAURI.dialog.save({ defaultPath: safe + '.orgrules', filters: [{ name: 'FilePilot', extensions: ['orgrules'] }] });
    if (!path) return;
    await invoke('export_rules', { path, text: JSON.stringify({ app: 'filepilot', schema_version: 1, rules: [r], dictionaries: S.doc.dictionaries }, null, 2) });
    toast(t('exportedOk'));
  } catch (e) { toast(String(e), true); }
}

function renderJsonCard(r) {
  S.jsonEdited = false;
  const ta = h('textarea', { id: 'json-ta', class: 'inp mono json-ta', spellcheck: false, rows: 16, 'aria-label': t('jsonTitle'),
    oninput: () => { S.jsonEdited = true; ta.classList.add('edited'); jsonError(''); const m = document.getElementById('json-state'); if (m) m.textContent = t('jsonUnapplied'); } });
  ta.value = ruleJson(r);
  return h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('jsonTitle')),
      h('span', { class: 'row' }, h('span', { id: 'json-state', class: 'muted small' }),
        h('button', { type: 'button', class: 'btn small', onclick: copyJson }, t('copy')),
        h('button', { type: 'button', class: 'btn small', onclick: exportRule }, t('exportRule')),
        h('button', { type: 'button', class: 'btn small primary', onclick: applyJson }, t('apply')))),
    h('div', { class: 'cbody' }, ta,
      h('p', { class: 'hint' }, t('jsonHelp')),
      h('p', { id: 'json-err', class: 'err' })));
}

// ---------- variabile: meniu cu descrieri și exemple ----------
const MONTHS = {
  ro: ['ianuarie', 'februarie', 'martie', 'aprilie', 'mai', 'iunie', 'iulie', 'august', 'septembrie', 'octombrie', 'noiembrie', 'decembrie'],
  en: ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'],
};
const pad2 = (n) => String(n).padStart(2, '0');

/// The same tokens as the core (timefmt::format): Romanian AAAA, AA, LLLL, LL, ZZ, HH, MM
/// (minutes), SS, or English YYYY, YY, MMMM, MM (month), DD, HH, mm, ss; a format uses one set.
const englishTokens = (f) => ['YY', 'DD', 'MMMM', 'mm', 'ss'].some((x) => f.includes(x)) || f.trim() === 'MM';
function fmtDate(ts, fmt) {
  const d = new Date(ts * 1000);
  const toks = englishTokens(fmt)
    ? [['YYYY', () => String(d.getFullYear()).padStart(4, '0')], ['YY', () => pad2(d.getFullYear() % 100)],
      ['MMMM', () => MONTHS[LANG === 'en' ? 'en' : 'ro'][d.getMonth()]], ['MM', () => pad2(d.getMonth() + 1)],
      ['DD', () => pad2(d.getDate())], ['HH', () => pad2(d.getHours())], ['mm', () => pad2(d.getMinutes())], ['ss', () => pad2(d.getSeconds())]]
    : [['AAAA', () => String(d.getFullYear()).padStart(4, '0')], ['AA', () => pad2(d.getFullYear() % 100)],
      ['LLLL', () => MONTHS[LANG === 'en' ? 'en' : 'ro'][d.getMonth()]], ['LL', () => pad2(d.getMonth() + 1)],
      ['ZZ', () => pad2(d.getDate())], ['HH', () => pad2(d.getHours())], ['MM', () => pad2(d.getMinutes())], ['SS', () => pad2(d.getSeconds())]];
  let out = '';
  for (let i = 0; i < fmt.length;) {
    const hit = toks.find(([k]) => fmt.startsWith(k, i));
    if (hit) { out += hit[1](); i += hit[0].length; } else { out += fmt[i]; i += 1; }
  }
  return out;
}

/// Date formats are written here with the Romanian tokens; the English window shows and
/// inserts the English ones (the core understands both).
const TOK_EN = { AAAA: 'YYYY', AA: 'YY', LLLL: 'MMMM', LL: 'MM', ZZ: 'DD', MM: 'mm', SS: 'ss' };
const fmtTok = (f) => (LANG === 'en' ? f.replace(/AAAA|AA|LLLL|LL|ZZ|MM|SS/g, (m) => TOK_EN[m]) : f);

/// Fișierul pe care se arată exemplele: primul din previzualizare sau unul inventat.
function sampleFile() {
  const f = S.pv.data && S.pv.data.files && S.pv.data.files[0];
  const now = Math.floor(Date.now() / 1000);
  if (f) {
    const folder = f.folder || '';
    return { name: f.name, folder, path: f.path || folder + '\\' + f.name, size: f.size, created: f.created ?? f.modified, modified: f.modified };
  }
  const r = rule();
  const folder = (r && r.folders[0] && r.folders[0].path) || 'C:\\Users\\…\\Downloads';
  return { name: 'factura.pdf', folder, path: folder + '\\factura.pdf', size: 245760, created: now, modified: now };
}
const stemOf = (n) => { const i = n.lastIndexOf('.'); return i > 0 ? n.slice(0, i) : n; };
const extOf = (n) => { const i = n.lastIndexOf('.'); return i > 0 ? n.slice(i + 1) : ''; };

/// Valoarea unui dicționar pentru fișier, ca în nucleu: prima intrare conținută în câmp.
function dictValue(d, f) {
  const hay = ({ name: stemOf(f.name), extension: extOf(f.name), path: f.path, folder: f.folder }[d.field] || '').toLowerCase();
  const hit = (d.entries || []).find(([k]) => String(k || '').split(',').map((x) => x.trim().toLowerCase()).some((x) => x && hay.includes(x)));
  return hit ? hit[1] : (d.default ?? '');
}

const DATE_PRESETS = [
  ['AAAA', 'fmtYear'], ['LL', 'fmtMonth'], ['LLLL', 'fmtMonthName'], ['AAAA-LL', 'fmtYearMonth'],
  ['AAAA-LL-ZZ', 'fmtDate'], ['ZZ.LL.AAAA', 'fmtDateRo'], ['AAAA-LL-ZZ HH-MM', 'fmtDateTime'],
];

function varGroups() {
  const f = sampleFile();
  const now = Math.floor(Date.now() / 1000);
  const groups = [[t('vgFile'), [
    ['{name}', t('varName'), stemOf(f.name)], ['{ext}', t('varExt'), extOf(f.name)], ['{file}', t('varFile'), f.name],
    ['{size}', t('varSize'), String(f.size)], ['{folder}', t('varFolder'), f.folder], ['{path}', t('varPath'), f.path],
  ]]];
  for (const [src, title, ts] of [['modified', t('vgModified'), f.modified], ['created', t('vgCreated'), f.created], ['now', t('vgNow'), now]]) {
    const items = DATE_PRESETS.map(([fmt, key]) => [`{${src}:${fmtTok(fmt)}}`, t(key), fmtDate(ts, fmtTok(fmt))]);
    items.push([`{${src}:${fmtTok('AAAA')}}\\{${src}:${fmtTok('LL')}}`, t('fmtYearMonthFolders'), fmtDate(ts, 'AAAA') + '\\' + fmtDate(ts, 'LL')]);
    groups.push([title, items]);
  }
  const dicts = Object.entries(S.doc.dictionaries || {});
  groups.push([t('vgDicts'), [...dicts.map(([n, d]) => [`{var:${n}}`, n, d.extract ? t('varFromText') : dictValue(d, f)]),
    [NEW_VAR, t('varNewFromRule'), '']]]);
  return groups;
}

/// The item of the variable menu that makes a new variable.
const NEW_VAR = '+';
let openPop = null;
function closeVarMenu() {
  if (!openPop) return;
  openPop.el.remove();
  document.removeEventListener('mousedown', openPop.outside, true);
  document.removeEventListener('keydown', openPop.key, true);
  const back = openPop.button;
  openPop = null;
  if (back && document.body.contains(back)) back.focus();
}

/// Butonul „{ } Variabilă”: un panou cu variabilele grupate, descriere și exemplu pe fișierul curent.
function varMenu(onPick) {
  const btn = h('button', { type: 'button', class: 'btn var-btn', 'aria-haspopup': 'true', title: t('insertVar') },
    h('span', { class: 'mono' }, '{ }'), t('variable'));
  btn.onclick = () => {
    if (openPop && openPop.button === btn) { closeVarMenu(); return; }
    closeVarMenu();
    const search = h('input', { class: 'inp', type: 'search', placeholder: t('searchVar'), 'aria-label': t('searchVar') });
    const list = h('div', { class: 'var-list', role: 'menu' });
    const fillList = () => {
      const q = search.value.trim().toLowerCase();
      const groups = varGroups()
        .map(([title, items]) => [title, items.filter(([tok, label, ex]) => !q || (tok + ' ' + label + ' ' + ex).toLowerCase().includes(q))])
        .filter(([, items]) => items.length);
      fill(list, groups.length ? groups.map(([title, items]) => h('div', { class: 'var-group' },
        h('div', { class: 'var-gtitle' }, title),
        items.map(([tok, label, ex]) => h('button', { type: 'button', class: 'var-item', role: 'menuitem', 'data-tok': tok,
          onclick: () => { closeVarMenu(); if (tok === NEW_VAR) newVarFromRule((n) => onPick(`{var:${n}}`)); else onPick(tok); } },
          h('span', { class: 'var-label' }, label),
          h('span', { class: 'var-ex' }, tok === NEW_VAR ? '' : ex || '—'),
          h('span', { class: 'var-tok mono' }, tok === NEW_VAR ? '' : tok))))) : h('p', { class: 'muted pad' }, t('noVarFound')));
    };
    search.oninput = fillList;
    search.onkeydown = (e) => {
      if (e.key === 'Enter') { e.preventDefault(); const first = list.querySelector('.var-item'); if (first) first.click(); }
      if (e.key === 'ArrowDown') { e.preventDefault(); const first = list.querySelector('.var-item'); if (first) first.focus(); }
    };
    list.onkeydown = (e) => {
      if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
      e.preventDefault();
      const items = [...list.querySelectorAll('.var-item')];
      const i = items.indexOf(document.activeElement);
      const j = e.key === 'ArrowDown' ? Math.min(items.length - 1, i + 1) : i - 1;
      if (j < 0) search.focus(); else items[j].focus();
    };
    const el = h('div', { class: 'var-pop' }, h('div', { class: 'var-head' }, search), list);
    fillList();
    document.body.append(el);
    const b = btn.getBoundingClientRect();
    const w = Math.min(460, window.innerWidth - 24);
    el.style.width = w + 'px';
    el.style.left = Math.max(12, Math.min(b.right - w, window.innerWidth - w - 12)) + 'px';
    const below = window.innerHeight - b.bottom - 16;
    if (below >= 260) { el.style.top = (b.bottom + 4) + 'px'; el.style.maxHeight = Math.min(420, below) + 'px'; }
    else { el.style.bottom = (window.innerHeight - b.top + 4) + 'px'; el.style.maxHeight = Math.min(420, b.top - 16) + 'px'; }
    const outside = (e) => { if (!el.contains(e.target) && e.target !== btn && !btn.contains(e.target)) closeVarMenu(); };
    const key = (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeVarMenu(); } };
    document.addEventListener('mousedown', outside, true);
    document.addEventListener('keydown', key, true);
    openPop = { el, button: btn, outside, key };
    search.focus();
  };
  return btn;
}

// ---------- validare: aceleași reguli ca nucleul, arătate lângă câmp ----------
const PLAIN_VARS = ['name', 'ext', 'file', 'folder', 'path', 'size'];

function varError(tok) {
  const k = tok.indexOf(':');
  const head = k < 0 ? tok : tok.slice(0, k);
  const arg = k < 0 ? null : tok.slice(k + 1);
  if (PLAIN_VARS.includes(head) && arg === null) return null;
  if (['created', 'modified', 'now'].includes(head) && arg !== null) return null;
  if (head === 'var' && arg) return arg in (S.doc.dictionaries || {}) ? null : t('errUnknownDict', { name: arg });
  return t('errUnknownVar', { v: '{' + tok + '}' });
}

/// Ca `Template::parse`: `{{` și `}}` sunt acolade literale.
function templateError(s) {
  s = s || '';
  for (let i = 0; i < s.length;) {
    const c = s[i];
    if (c === '{') {
      if (s[i + 1] === '{') { i += 2; continue; }
      const j = s.indexOf('}', i + 1);
      if (j < 0) return t('errUnclosed');
      const e = varError(s.slice(i + 1, j));
      if (e) return e;
      i = j + 1;
    } else if (c === '}') {
      if (s[i + 1] === '}') { i += 2; continue; }
      return t('errUnmatched');
    } else i += 1;
  }
  return null;
}

/// Problemele regulii: [obiect, câmp, mesaj, nivel]. `error` = daemonul ar refuza regula
/// (sau salvarea ar eșua); `warn` = ar merge, dar probabil nu face ce vrei.
function ruleIssues(r) {
  const out = [];
  const err = (o, f, m) => out.push([o, f, m, 'error']);
  const warn = (o, f, m) => out.push([o, f, m, 'warn']);
  if (!r.folders.length) err(r, 'folders', t('issNoFolders'));
  const missing = new Set((S.pv.data && S.pv.data.unavailable) || []);
  for (const f of r.folders) {
    if (!f.path.trim()) err(f, 'path', t('issEmptyFolder'));
    else if (missing.has(f.path)) warn(f, 'path', t('issFolderMissing'));
  }
  const walk = (n) => {
    if (n.type === 'all' || n.type === 'any') {
      if (!n.items.length) err(n, 'items', t('issEmptyGroup'));
      n.items.forEach(walk);
    } else if (n.type === 'text' && n.op !== 'is_nothing' && !String(n.value || '').trim()) {
      warn(n, 'value', t('issEmptyValue'));
    } else if (n.type === 'var' && !S.doc.dictionaries[n.name]) {
      err(n, 'name', n.name ? t('issVarUnknown', { name: n.name }) : t('issVarNone'));
    }
  };
  walk(r.condition);
  if (!r.actions.length) err(r, 'actions', t('issNoActions'));
  let removed = false;
  r.actions.forEach((a) => {
    if (removed) err(a, 'type', t('issAfterRemoval'));
    const tpl = (field, required, emptyKey) => {
      const v = a[field] || '';
      if (!v.trim()) { if (required) err(a, field, t(emptyKey)); return; }
      const e = templateError(v);
      if (e) err(a, field, e);
    };
    switch (a.type) {
      case 'move': case 'copy': case 'extract': tpl('dest', true, 'issEmptyDest'); break;
      case 'rename': tpl('name', true, 'issEmptyName'); break;
      case 'change_extension': tpl('ext', true, 'issEmptyExt'); break;
      case 'command': tpl('line', true, 'issEmptyCommand'); break;
      case 'confirm': if (a === r.actions[r.actions.length - 1]) warn(a, 'type', t('issConfirmLast')); break;
      case 'run': {
        tpl('program', true, 'issEmptyProgram');
        const bad = (a.args || []).map(templateError).find(Boolean);
        if (bad) err(a, 'args', bad);
        break;
      }
    }
    if (a.type === 'trash' || a.type === 'delete') removed = true;
  });
  return out;
}

const firstError = (r) => { const e = ruleIssues(r).find((x) => x[3] === 'error'); return e ? e[2] : null; };

/// Câmpurile care își arată problema: (obiect, câmp) → element de marcat + loc pentru mesaj.
let ISSUE_SLOTS = [];
function issueSlot(obj, field, markEl) {
  const p = h('p', { class: 'field-err', hidden: true, role: 'status' });
  ISSUE_SLOTS.push({ obj, field, markEl, p });
  return p;
}

function refreshIssues() {
  const r = rule();
  const issues = r ? ruleIssues(r) : [];
  for (const s of ISSUE_SLOTS) {
    const hit = issues.find(([o, f]) => o === s.obj && f === s.field);
    s.p.hidden = !hit;
    s.p.textContent = hit ? hit[2] : '';
    s.p.classList.toggle('warn-text', !!hit && hit[3] === 'warn');
    if (s.markEl) {
      s.markEl.classList.toggle('invalid', !!hit && hit[3] === 'error');
      s.markEl.classList.toggle('warned', !!hit && hit[3] === 'warn');
    }
  }
  const banner = document.getElementById('draft-activate');
  if (banner && r) {
    const e = firstError(r);
    banner.disabled = !!e;
    const why = document.getElementById('draft-why');
    if (why) { why.hidden = !e; why.textContent = e ? t('cannotEnable', { msg: e }) : ''; }
  }
}

/// Activează o regulă doar dacă daemonul ar accepta-o.
function setEnabled(r, on) {
  if (on) {
    const e = firstError(r);
    if (e) { toast(t('cannotEnable', { msg: e }), true); render(); return false; }
  }
  r.enabled = on;
  markDirty();
  render();
  return true;
}

// ---------- regulă nouă din șablon ----------
const SORT_DICT = {
  ro: ['TipFisier', [['Documente', 'pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt csv'], ['Imagini', 'jpg jpeg png gif webp heic bmp svg tif'],
    ['Video', 'mp4 mkv avi mov webm wmv'], ['Muzică', 'mp3 flac wav m4a ogg aac'], ['Arhive', 'zip rar 7z tar gz'], ['Programe', 'exe msi']], 'Altele'],
  en: ['FileType', [['Documents', 'pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt csv'], ['Images', 'jpg jpeg png gif webp heic bmp svg tif'],
    ['Video', 'mp4 mkv avi mov webm wmv'], ['Music', 'mp3 flac wav m4a ogg aac'], ['Archives', 'zip rar 7z tar gz'], ['Programs', 'exe msi']], 'Other'],
};

const extIs = (v) => ({ type: 'text', field: 'extension', op: 'is', value: v });
// Folder names created by the templates follow the interface language.
const tplDir = (ro, en) => (LANG === 'en' ? en : ro);

const TEMPLATES = [
  { key: 'empty', make: () => defaultRule() },
  { key: 'sort', make: (dl) => {
    const [name, groups, other] = SORT_DICT[LANG === 'en' ? 'en' : 'ro'];
    if (!S.doc.dictionaries[name]) {
      S.doc.dictionaries[name] = { field: 'extension', entries: groups.map(([cat, exts]) => [exts.split(' ').join(', '), cat]), default: other };
    }
    return { name: t('tplSort'), enabled: false, hidden: false, continue_after_move: false,
      folders: dl ? [{ path: dl, recursive: false }] : [], condition: { type: 'all_files' },
      actions: [{ type: 'move', dest: `{var:${name}}`, conflict: 'dedupe', keep_structure: false, remove_empty: false }] };
  } },
  { key: 'old', make: (dl) => ({ name: t('tplOld'), enabled: false, hidden: false, continue_after_move: false,
    folders: dl ? [{ path: dl, recursive: false }] : [],
    condition: { type: 'all', items: [{ type: 'date', field: 'modified', op: 'older_than_days', value: 30 }] },
    actions: [{ type: 'move', dest: tplDir('Arhiva', 'Archive') + `\\{modified:${fmtTok('AAAA-LL')}}`, conflict: 'rename', keep_structure: false, remove_empty: false }] }) },
  { key: 'invoices', make: (dl) => ({ name: t('tplInvoices'), enabled: false, hidden: false, continue_after_move: false,
    folders: dl ? [{ path: dl, recursive: false }] : [],
    condition: { type: 'all', items: [extIs('pdf'), { type: 'any', items: [
      { type: 'text', field: 'name', op: 'contains', value: 'factur' }, { type: 'text', field: 'name', op: 'contains', value: 'invoice' }] }] },
    actions: [{ type: 'move', dest: tplDir('Facturi', 'Invoices') + `\\{modified:${fmtTok('AAAA')}}\\{modified:${fmtTok('LL')}}`, conflict: 'rename', keep_structure: false, remove_empty: false }] }) },
  { key: 'cleanup', make: (dl) => ({ name: t('tplCleanup'), enabled: false, hidden: false, continue_after_move: false,
    folders: dl ? [{ path: dl, recursive: false }] : [],
    condition: { type: 'any', items: [extIs('tmp'), extIs('bak'), extIs('old')] },
    actions: [{ type: 'trash', remove_empty: false }] }) },
  { key: 'dupes', make: (dl) => ({ name: t('tplDupes'), enabled: false, hidden: false, continue_after_move: false,
    folders: dl ? [{ path: dl, recursive: false }] : [],
    condition: { type: 'all', items: [{ type: 'duplicate', is: true }] },
    actions: [{ type: 'confirm' }, { type: 'trash', remove_empty: false }] }) },
  { key: 'extract', make: (dl) => ({ name: t('tplExtract'), enabled: false, hidden: false, continue_after_move: false,
    folders: dl ? [{ path: dl, recursive: false }] : [],
    condition: { type: 'any', items: [extIs('zip'), extIs('rar'), extIs('7z')] },
    actions: [{ type: 'extract', dest: EXTRACT_HERE, conflict: 'rename' }, { type: 'trash', remove_empty: false }] }) },
  { key: 'backup', make: () => ({ name: t('tplBackup'), enabled: false, hidden: false, continue_after_move: true,
    folders: [], condition: { type: 'all_files' },
    actions: [{ type: 'copy', dest: '', conflict: 'overwrite', keep_structure: true, then_target: false }] }) },
];
const TPL_TEXT = { empty: ['tplEmpty', 'tplEmptyDesc'], sort: ['tplSort', 'tplSortDesc'], old: ['tplOld', 'tplOldDesc'],
  invoices: ['tplInvoices', 'tplInvoicesDesc'], cleanup: ['tplCleanup', 'tplCleanupDesc'], dupes: ['tplDupes', 'tplDupesDesc'],
  extract: ['tplExtract', 'tplExtractDesc'],
  backup: ['tplBackup', 'tplBackupDesc'] };

function templateModal() {
  return new Promise((resolve) => {
    const root = document.getElementById('modal-root');
    const done = (v) => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); done(null); } };
    window.addEventListener('keydown', onKey, true);
    fill(root, h('div', { class: 'overlay', onclick: (e) => { if (e.target === e.currentTarget) done(null); } },
      h('div', { class: 'modal wide', role: 'dialog', 'aria-modal': 'true', 'aria-label': t('newRule') },
        h('h2', {}, t('newRule')),
        h('div', { class: 'tpl-grid' }, TEMPLATES.map(({ key }) => h('button', { type: 'button', class: 'tpl-card', onclick: () => done(key) },
          h('strong', {}, t(TPL_TEXT[key][0])), h('span', { class: 'muted small' }, t(TPL_TEXT[key][1]))))),
        h('div', { class: 'row end' }, h('button', { type: 'button', class: 'btn', onclick: () => done(null) }, t('cancel'))))));
    const first = root.querySelector('.tpl-card');
    if (first) first.focus();
  });
}

async function downloadsDir() {
  try { return (await TAURI.path.downloadDir()) || ''; } catch (_) { return ''; }
}

function uniqueRuleName(base) {
  const names = new Set(S.doc.rules.map((x) => x.name));
  if (!names.has(base)) return base;
  for (let i = 2; ; i++) if (!names.has(`${base} (${i})`)) return `${base} (${i})`;
}

async function newRuleFromTemplate() {
  const key = await templateModal();
  if (!key) return;
  const tpl = TEMPLATES.find((x) => x.key === key);
  const dl = key === 'empty' || key === 'backup' ? '' : await downloadsDir();
  const r = tpl.make(dl);
  r.name = uniqueRuleName(r.name);
  S.doc.rules.push(r);
  S.sel = S.doc.rules.length - 1;
  S.pv.data = null;
  markDirty();
  render();
  runPreview();
}

/// Regulă oprită: explică de ce nu face nimic și oferă activarea, după verificare.
function draftBanner(r) {
  const e = firstError(r);
  return h('section', { class: 'card draft' },
    h('div', { class: 'cbody' },
      h('strong', {}, t('draftTitle')),
      h('p', { class: 'muted' }, t('draftBody')),
      h('p', { id: 'draft-why', class: 'field-err', hidden: !e }, e ? t('cannotEnable', { msg: e }) : ''),
      h('div', { class: 'row' },
        h('button', { id: 'draft-activate', type: 'button', class: 'btn primary', disabled: !!e,
          onclick: async () => { if (setEnabled(r, true)) await save(); } }, t('activateAndSave')))));
}

// ---------- meniu la clic dreapta ----------
let openCtx = null;
function closeCtx() {
  if (!openCtx) return;
  openCtx.el.remove();
  for (const [ev, fn, opt] of openCtx.listeners) (ev === 'resize' || ev === 'blur' ? window : document).removeEventListener(ev, fn, opt);
  const back = openCtx.back;
  openCtx = null;
  if (back && document.body.contains(back)) back.focus();
}

/// `items`: { label, run, danger, disabled } sau '-' pentru separator.
// ---------- (i) explicații ----------
/// Buton (i) care deschide o explicație lângă el. `body`: text sau listă de [termen, descriere].
function info(title, body) {
  const btn = h('button', { type: 'button', class: 'info-btn', title: t('moreInfo'), 'aria-label': t('moreInfo'), 'aria-expanded': 'false',
    onclick: (ev) => { ev.preventDefault(); ev.stopPropagation(); openInfo(btn, title, body); } }, 'i');
  return btn;
}

let infoPop = null;
function closeInfo() {
  if (!infoPop) return;
  infoPop.el.remove();
  infoPop.anchor.setAttribute('aria-expanded', 'false');
  document.removeEventListener('mousedown', onInfoOutside, true);
  window.removeEventListener('keydown', onInfoKey, true);
  window.removeEventListener('scroll', closeInfo, true);
  window.removeEventListener('resize', closeInfo);
  infoPop = null;
}
function onInfoOutside(e) { if (infoPop && !infoPop.el.contains(e.target) && e.target !== infoPop.anchor) closeInfo(); }
function onInfoKey(e) { if (e.key === 'Escape') { e.preventDefault(); const a = infoPop && infoPop.anchor; closeInfo(); if (a) a.focus(); } }

function openInfo(anchor, title, body) {
  const same = infoPop && infoPop.anchor === anchor;
  closeInfo();
  if (same) return;
  const content = Array.isArray(body)
    ? h('dl', { class: 'info-list' }, body.map(([k, v]) => [h('dt', {}, k), h('dd', {}, v)]))
    : h('p', {}, body);
  const el = h('div', { class: 'info-pop', role: 'dialog', 'aria-label': title || t('moreInfo') }, title ? h('strong', {}, title) : null, content);
  document.body.appendChild(el);
  const r = anchor.getBoundingClientRect();
  const w = el.offsetWidth, ht = el.offsetHeight;
  el.style.left = Math.max(8, Math.min(r.left - 12, innerWidth - w - 8)) + 'px';
  el.style.top = (r.bottom + 6 + ht > innerHeight && r.top - ht - 6 > 0 ? r.top - ht - 6 : r.bottom + 6) + 'px';
  anchor.setAttribute('aria-expanded', 'true');
  infoPop = { el, anchor };
  document.addEventListener('mousedown', onInfoOutside, true);
  window.addEventListener('keydown', onInfoKey, true);
  window.addEventListener('scroll', closeInfo, true);
  window.addEventListener('resize', closeInfo);
}

const conflictInfo = () => info(t('ifExists'), [
  [t('conflictRename'), t('infoConflictRename')],
  [t('conflictDedupe'), t('infoConflictDedupe')],
  [t('conflictSkip'), t('infoConflictSkip')],
  [t('conflictOverwrite'), t('infoConflictOverwrite')],
]);
const actionInfo = () => info(t('actionType'), ACTION_TYPES.map(([v, k]) => [t(k), t('infoAct_' + v)]));

function contextMenu(ev, items) {
  ev.preventDefault();
  closeCtx();
  closeVarMenu();
  const el = h('div', { class: 'ctx-menu', role: 'menu' }, items.filter(Boolean).map((it) => it === '-'
    ? h('div', { class: 'ctx-sep', role: 'separator' })
    : h('button', { type: 'button', class: 'ctx-item' + (it.danger ? ' danger' : ''), role: 'menuitem', disabled: !!it.disabled,
      onclick: () => { closeCtx(); it.run(); } }, it.label)));
  document.body.append(el);
  // De la tastatură (tasta Meniu, Shift+F10) evenimentul nu are coordonate: lângă element.
  let x = ev.clientX, y = ev.clientY;
  if (!x && !y && ev.currentTarget && ev.currentTarget.getBoundingClientRect) {
    const r = ev.currentTarget.getBoundingClientRect(); x = r.left + 24; y = r.bottom - 4;
  }
  const w = el.offsetWidth, hgt = el.offsetHeight;
  el.style.left = Math.max(4, Math.min(x, window.innerWidth - w - 4)) + 'px';
  el.style.top = Math.max(4, Math.min(y, window.innerHeight - hgt - 4)) + 'px';
  const outside = (e) => { if (!el.contains(e.target)) closeCtx(); };
  const key = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeCtx(); return; }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const btns = [...el.querySelectorAll('.ctx-item:not(:disabled)')];
    const i = btns.indexOf(document.activeElement);
    const j = e.key === 'ArrowDown' ? (i + 1) % btns.length : (i - 1 + btns.length) % btns.length;
    btns[j].focus();
  };
  const away = () => closeCtx();
  const listeners = [['mousedown', outside, true], ['keydown', key, true], ['wheel', away, true], ['resize', away, false], ['blur', away, false]];
  for (const [e, fn, opt] of listeners) (e === 'resize' || e === 'blur' ? window : document).addEventListener(e, fn, opt);
  openCtx = { el, listeners, back: ev.currentTarget };
  const first = el.querySelector('.ctx-item:not(:disabled)');
  if (first) first.focus();
}

// ---------- dicționare: folosire, redenumire, duplicare, ștergere ----------
const TPL_FIELDS = ['dest', 'name', 'ext', 'program', 'line'];

/// Regulile care folosesc `{var:name}` (în orice câmp cu variabile).
/// Calls `fn` for every leaf of a condition tree.
function eachLeaf(n, fn) {
  if (n.type === 'all' || n.type === 'any') n.items.forEach((x) => eachLeaf(x, fn));
  else fn(n);
}

function dictUsers(name) {
  const tok = `{var:${name}}`;
  return S.doc.rules.filter((r) => {
    let inCond = false;
    eachLeaf(r.condition, (n) => { if (n.type === 'var' && n.name === name) inCond = true; });
    return inCond || r.actions.some((a) =>
      TPL_FIELDS.some((f) => typeof a[f] === 'string' && a[f].includes(tok)) || (a.args || []).some((x) => x.includes(tok)));
  });
}

/// Înlocuiește `{var:vechi}` cu `{var:nou}` în acțiunile regulilor date.
function retargetDict(rules, from, to) {
  const a1 = `{var:${from}}`, a2 = `{var:${to}}`;
  let n = 0;
  for (const r of rules) for (const a of r.actions) {
    for (const f of TPL_FIELDS) if (typeof a[f] === 'string' && a[f].includes(a1)) { a[f] = a[f].split(a1).join(a2); n++; }
    if (a.args) a.args = a.args.map((x) => (x.includes(a1) ? (n++, x.split(a1).join(a2)) : x));
  }
  for (const r of rules) eachLeaf(r.condition, (c) => { if (c.type === 'var' && c.name === from) { c.name = to; n++; } });
  return n;
}

function uniqueDictName(base) {
  if (!S.doc.dictionaries[base]) return base;
  // Numele apare în șabloane ({var:Nume_2}), deci fără spații și paranteze.
  for (let i = 2; ; i++) if (!S.doc.dictionaries[`${base}_${i}`]) return `${base}_${i}`;
}

/// Redenumește păstrând ordinea și actualizează regulile care îl folosesc.
function renameDict(from, to) {
  to = (to || '').trim();
  if (!to || to === from) return true;
  if (S.doc.dictionaries[to]) { toast(t('dictExists', { name: to }), true); return false; }
  if (/[{}:]/.test(to)) { toast(t('dictBadName'), true); return false; }
  const rebuilt = {};
  for (const [k, v] of Object.entries(S.doc.dictionaries)) rebuilt[k === from ? to : k] = v;
  S.doc.dictionaries = rebuilt;
  const n = retargetDict(S.doc.rules, from, to);
  if (S.dictSel === from) S.dictSel = to;
  markDirty(); render();
  if (n) toast(t('dictRenamedUsed', { n }));
  return true;
}

function duplicateDict(name) {
  const copy = uniqueDictName(name);
  const rebuilt = {};
  for (const [k, v] of Object.entries(S.doc.dictionaries)) {
    rebuilt[k] = v;
    if (k === name) rebuilt[copy] = clone(v);
  }
  S.doc.dictionaries = rebuilt; S.dictSel = copy; markDirty(); render();
}

async function deleteDict(name) {
  const users = dictUsers(name);
  const msg = users.length
    ? t('confirmDeleteDictUsed', { name, n: users.length, rules: users.map((r) => r.name).join(', ') })
    : t('confirmDeleteDict', { name });
  if (!(await confirmModal(msg, t('delete')))) return;
  delete S.doc.dictionaries[name];
  if (S.dictSel === name) S.dictSel = null;
  markDirty(); render();
}

// ---------- dicționare: export, import ----------
async function exportDicts(names) {
  try {
    const safe = (names.length === 1 ? names[0] : t('dictionaries')).replace(/[\\/:*?"<>|]+/g, '_');
    const path = await TAURI.dialog.save({ defaultPath: safe + '.orgrules', filters: [{ name: 'FilePilot', extensions: ['orgrules'] }] });
    if (!path) return;
    const dictionaries = {};
    for (const n of names) dictionaries[n] = S.doc.dictionaries[n];
    await invoke('export_rules', { path, text: JSON.stringify({ app: 'filepilot', schema_version: 1, rules: [], dictionaries }, null, 2) });
    toast(t('exportedOk'));
  } catch (e) { toast(String(e), true); }
}

const sameDict = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/// Adaugă dicționarele primite. Unul identic cu cel existent nu se dublează; unul cu același
/// nume dar alt conținut primește un nume nou, iar `rules` (regulile importate odată cu el)
/// sunt mutate pe noul nume. Întoarce [adăugate, redenumiri].
function mergeDicts(incoming, rules) {
  let added = 0;
  const renamed = [];
  for (const [k, v] of Object.entries(incoming || {})) {
    if (S.doc.dictionaries[k] && sameDict(S.doc.dictionaries[k], v)) continue;
    // Același conținut sub alt nume (de la un import anterior): îl refolosim.
    const twin = Object.keys(S.doc.dictionaries).find((n) => sameDict(S.doc.dictionaries[n], v));
    if (twin) { retargetDict(rules || [], k, twin); continue; }
    const n = S.doc.dictionaries[k] ? uniqueDictName(k) : k;
    S.doc.dictionaries[n] = v;
    added++;
    if (n !== k) { renamed.push([k, n]); retargetDict(rules || [], k, n); }
  }
  return [added, renamed];
}

async function importDicts() {
  try {
    const path = await TAURI.dialog.open({ multiple: false, filters: [{ name: 'FilePilot', extensions: ['orgrules', 'json'] }] });
    if (!path) return;
    const incoming = JSON.parse(await invoke('read_rules_file', { path }));
    const [added, renamed] = mergeDicts(incoming.dictionaries, []);
    if (!added) { toast(t('dictImportNone')); return; }
    if (!S.doc.dictionaries[S.dictSel]) S.dictSel = Object.keys(incoming.dictionaries)[0];
    markDirty(); render();
    toast(t('dictImported', { n: added }) + (renamed.length ? ' ' + t('dictImportRenamed', { list: renamed.map(([a, b]) => `${a} → ${b}`).join(', ') }) : ''));
  } catch (e) { toast(String(e), true); }
}

// ---------- dicționare: JSON ----------
/// JSON lizibil: fiecare pereche [cheie, valoare] pe un singur rând.
const dictJson = (name, d) => JSON.stringify({ [name]: d }, null, 2)
  .replace(/\[\s*("(?:[^"\\]|\\.)*"),\s*("(?:[^"\\]|\\.)*")\s*\]/g, '[$1, $2]');

function syncDictJson() {
  const ta = document.getElementById('dict-json-ta');
  const d = S.dictSel ? S.doc.dictionaries[S.dictSel] : null;
  if (ta && d && !S.dictJsonEdited) ta.value = dictJson(S.dictSel, d);
}

function dictShapeError(d) {
  if (!d || typeof d !== 'object' || Array.isArray(d)) return t('dictJsonShape');
  if (!TEXT_FIELDS.includes(d.field)) return t('dictJsonField', { list: TEXT_FIELDS.join(', ') });
  if (!Array.isArray(d.entries) || !d.entries.every((e) => Array.isArray(e) && e.length === 2 && e.every((x) => typeof x === 'string'))) return t('dictJsonEntries');
  if (!(d.default === null || d.default === undefined || typeof d.default === 'string')) return t('dictJsonDefault');
  if (d.extract !== undefined && d.extract !== null
    && (typeof d.extract !== 'object' || !VAR_KINDS.includes(d.extract.kind))) return t('dictJsonExtract', { list: VAR_KINDS.join(', ') });
  return null;
}

/// Acceptă `{ "Nume": {...} }` (poate redenumi), un dicționar simplu `{ field, entries, default }`
/// sau un fișier .orgrules cu exact un dicționar.
function applyDictJson() {
  const ta = document.getElementById('dict-json-ta');
  const err = (m) => { const el = document.getElementById('dict-json-err'); if (el) el.textContent = m || ''; };
  let obj;
  try { obj = JSON.parse(ta.value); } catch (e) { err(t('jsonInvalid', { msg: e.message })); return; }
  let name = S.dictSel, d = obj;
  if (obj && obj.dictionaries && typeof obj.dictionaries === 'object') obj = obj.dictionaries;
  if (obj && typeof obj === 'object' && !Array.isArray(obj) && !('field' in obj)) {
    const keys = Object.keys(obj);
    if (keys.length !== 1) { err(t('dictJsonMany')); return; }
    [name] = keys; d = obj[name];
  }
  const shape = dictShapeError(d);
  if (shape) { err(shape); return; }
  d = { field: d.field, entries: d.entries, default: d.default ?? null, ...(d.extract ? { extract: d.extract } : {}) };
  if (name !== S.dictSel) {
    if (S.doc.dictionaries[name]) { err(t('dictExists', { name })); return; }
    if (!renameDict(S.dictSel, name)) return;
  }
  S.doc.dictionaries[name] = d;
  S.dictJsonEdited = false;
  markDirty(); render();
  toast(t('jsonApplied'));
}

async function copyText(ta) {
  try { await navigator.clipboard.writeText(ta.value); }
  catch (_) { ta.select(); document.execCommand('copy'); }
  toast(t('copied'));
}

function renderDictJsonCard(name, d, bare) {
  S.dictJsonEdited = false;
  const ta = h('textarea', { id: 'dict-json-ta', class: 'inp mono json-ta', spellcheck: false, rows: 12, 'aria-label': t('dictJsonTitle'),
    oninput: () => {
      S.dictJsonEdited = true; ta.classList.add('edited');
      const e = document.getElementById('dict-json-err'); if (e) e.textContent = '';
      const m = document.getElementById('dict-json-state'); if (m) m.textContent = t('jsonUnapplied');
    } });
  ta.value = dictJson(name, d);
  const tools = h('span', { class: 'row' }, h('span', { id: 'dict-json-state', class: 'muted small' }),
    h('button', { type: 'button', class: 'btn small', onclick: () => copyText(ta) }, t('copy')),
    h('button', { type: 'button', class: 'btn small', onclick: () => exportDicts([name]) }, t('exportDict')),
    h('button', { type: 'button', class: 'btn small primary', onclick: applyDictJson }, t('apply')));
  const body = h('div', { class: 'cbody' }, bare ? h('div', { class: 'row end' }, tools) : null, ta,
    h('p', { class: 'hint' }, t('dictJsonHelp')),
    h('p', { id: 'dict-json-err', class: 'err' }));
  if (bare) return body;
  return h('section', { class: 'card' }, h('div', { class: 'ch' }, h('span', {}, t('dictJsonTitle')), tools), body);
}

// ---------- reguli: meniul de clic dreapta ----------
function ruleMenu(ev, i) {
  const x = S.doc.rules[i];
  const sel = () => { S.sel = i; S.pv.data = null; };
  contextMenu(ev, [
    { label: x.enabled ? t('ctxDisable') : t('ctxEnable'), run: () => { sel(); setEnabled(x, !x.enabled); } },
    { label: t('duplicate'), run: () => { const c = clone(x); delete c.id; c.name = uniqueRuleName(x.name + ' (2)'); S.doc.rules.splice(i + 1, 0, c); S.sel = i + 1; markDirty(); render(); runPreview(); } },
    { label: t('exportRule'), run: () => { sel(); render(); exportRule(); } },
    '-',
    { label: t('moveUp'), disabled: i === 0, run: () => { const j = moveItem(S.doc.rules, i, -1); if (S.sel === i) S.sel = j; else if (S.sel === j) S.sel = i; markDirty(); render(); } },
    { label: t('moveDown'), disabled: i === S.doc.rules.length - 1, run: () => { const j = moveItem(S.doc.rules, i, 1); if (S.sel === i) S.sel = j; else if (S.sel === j) S.sel = i; markDirty(); render(); } },
    '-',
    { label: t('deleteRule'), danger: true, run: async () => {
      if (!(await confirmModal(t('confirmDeleteRule', { name: x.name }), t('delete')))) return;
      S.doc.rules.splice(i, 1); S.sel = Math.max(0, Math.min(S.sel, S.doc.rules.length - 1)); markDirty(); render();
    } },
  ]);
}

function dictMenu(ev, name) {
  contextMenu(ev, [
    { label: t('ctxRename'), run: () => { S.dictSel = name; render(); const el = document.querySelector('.dict-name'); if (el) { el.focus(); el.select(); } } },
    { label: t('duplicate'), run: () => duplicateDict(name) },
    { label: t('exportDict'), run: () => exportDicts([name]) },
    { label: t('ctxCopyToken', { tok: `{var:${name}}` }), run: async () => {
      try { await navigator.clipboard.writeText(`{var:${name}}`); toast(t('copied')); } catch (_) { toast(t('copyFailed'), true); }
    } },
    '-',
    { label: t('deleteDict'), danger: true, run: () => deleteDict(name) },
  ]);
}

// ---------- model implicit ----------
const defaultLeaf = () => ({ type: 'text', field: 'extension', op: 'is', value: '' });
const defaultRule = () => ({
  name: t('newRuleName'), enabled: false, hidden: false, continue_after_move: true,
  folders: [], condition: { type: 'all', items: [defaultLeaf()] }, actions: [],
});
/// Programul rulat e chiar fișierul găsit de regulă (`{path}`, `start "" "{path}"`): un
/// executabil descărcat din greșeală ar porni singur.
function runsMatchedFile(s) {
  const x = String(s || '').trim().toLowerCase().replace(/^"/, '');
  return /^(start\s+(""\s+)?"?|call\s+"?|cmd(\.exe)?\s+\/c\s+"?)?\{(path|file|name|folder)\b/.test(x);
}

function defaultAction(type) {
  switch (type) {
    case 'move': return { type, dest: '', conflict: 'rename', keep_structure: false, remove_empty: false };
    case 'copy': return { type, dest: '', conflict: 'rename', keep_structure: false, then_target: false };
    case 'rename': return { type, name: '{name}', conflict: 'rename' };
    case 'change_extension': return { type, ext: '' };
    case 'trash': return { type, remove_empty: false };
    case 'delete': return { type, remove_empty: false };
    case 'run': return { type, program: '', args: [] };
    case 'command': return { type, line: '' };
    case 'confirm': return { type };
    case 'extract': return { type, dest: EXTRACT_HERE, conflict: 'rename' };
  }
}

/// Default extraction folder: next to the archive, named after it.
const EXTRACT_HERE = '{folder}\\{name}';

// ---------- șabloane: inserare de variabile ----------
function tplVariables() {
  const base = [
    ['{name}', 'varName'], ['{ext}', 'varExt'], ['{file}', 'varFile'], ['{folder}', 'varFolder'],
    ['{path}', 'varPath'], ['{size}', 'varSize'],
    ['{created:AAAA}', 'varCreatedYear'], ['{created:LL}', 'varCreatedMonth'], ['{created:LLLL}', 'varCreatedMonthName'],
    ['{created:ZZ}', 'varCreatedDay'], ['{created:AAAA-LL-ZZ}', 'varCreatedFull'],
    ['{modified:AAAA}', 'varModifiedYear'], ['{modified:LL}', 'varModifiedMonth'],
    ['{modified:AAAA-LL-ZZ}', 'varModifiedFull'], ['{now:AAAA-LL-ZZ}', 'varNowFull'],
  ].map(([tok, key]) => [fmtTok(tok), `${t(key)}  ${fmtTok(tok)}`]);
  const dicts = Object.keys(S.doc.dictionaries).map((n) => [`{var:${n}}`, `${t('varDict', { name: n })}  {var:${n}}`]);
  return base.concat(dicts);
}

/// Partea fixă a unei căi cu variabile: „D:\\Docs\\{modified:AAAA}” → „D:\\Docs”.
function literalPrefix(v) {
  const i = (v || '').indexOf('{');
  return (i < 0 ? v || '' : v.slice(0, i)).replace(/[\\/]+$/, '');
}

/// Înlocuiește partea fixă cu calea aleasă și păstrează variabilele de după ea.
function withPickedBase(cur, picked) {
  const i = (cur || '').indexOf('{');
  if (i < 0) return picked;
  const tail = cur.slice(i);
  return picked.replace(/[\\/]+$/, '') + '\\' + tail;
}

async function browsePath(kind, current) {
  const base = literalPrefix(current);
  const opts = kind === 'dir'
    ? { directory: true, multiple: false, title: t('pickFolder') }
    : { multiple: false, title: t('pickProgram'), filters: [{ name: t('programs'), extensions: ['exe', 'bat', 'cmd', 'ps1', 'com'] }, { name: t('allFiles'), extensions: ['*'] }] };
  if (base) opts.defaultPath = base;
  try { return await TAURI.dialog.open(opts); } catch (e) { toast(String(e), true); return null; }
}

function tplInput(value, onchange, label, multiline, browse) {
  const input = h(multiline ? 'textarea' : 'input', {
    class: 'inp mono' + (multiline ? ' area' : ''), 'aria-label': label, spellcheck: false,
    value: value ?? '', ...(multiline ? { rows: 3 } : { type: 'text' }),
    oninput: (e) => onchange(e.target.value),
  });
  const sel = varMenu((tok) => {
    const st = input.selectionStart ?? input.value.length;
    const en = input.selectionEnd ?? st;
    input.value = input.value.slice(0, st) + tok + input.value.slice(en);
    const pos = st + tok.length;
    input.focus();
    input.setSelectionRange(pos, pos);
    onchange(input.value);
  });
  const pick = browse ? h('button', { type: 'button', class: 'btn icon-only', title: browse === 'dir' ? t('browseFolder') : t('browseFile'),
    'aria-label': browse === 'dir' ? t('browseFolder') : t('browseFile'),
    onclick: async () => {
      const p = await browsePath(browse, input.value);
      if (!p) return;
      input.value = browse === 'dir' ? withPickedBase(input.value, p) : p;
      onchange(input.value);
    } }, icon(browse === 'dir' ? 'folder' : 'file')) : null;
  return h('div', { class: 'tpl' }, input, pick, sel);
}

// ---------- condiții ----------
const TEXT_FIELDS = ['name', 'extension', 'path', 'folder', 'content'];
const FIELD_LABEL = { name: 'fieldName', extension: 'fieldExtension', path: 'fieldPath', folder: 'fieldFolder', content: 'fieldContent',
  size: 'fieldSize', created: 'fieldCreated', modified: 'fieldModified', duplicate: 'fieldDuplicate', var: 'fieldVar' };
const TEXT_OPS = [['is', 'opIs'], ['is_not', 'opIsNot'], ['contains', 'opContains'], ['does_not_contain', 'opDoesNotContain'],
  ['is_nothing', 'opIsNothing'], ['starts_with', 'opStartsWith'], ['ends_with', 'opEndsWith']];
const DATE_OPS = [['older_than_days', 'opOlderThanDays'], ['within_days', 'opWithinDays'], ['before', 'opBefore'], ['after', 'opAfter']];

function leafField(n) {
  return n.type === 'text' ? n.field : n.type === 'size' ? 'size' : n.type === 'duplicate' ? 'duplicate' : n.type === 'var' ? 'var' : n.field; // date: created|modified
}

function changeLeafField(node, field) {
  const keep = TEXT_FIELDS.includes(field) && node.type === 'text';
  for (const k of Object.keys(node)) delete node[k];
  if (TEXT_FIELDS.includes(field)) Object.assign(node, keep ? {} : {}, { type: 'text', field, op: field === 'content' ? 'contains' : 'is', value: '' });
  else if (field === 'size') Object.assign(node, { type: 'size', op: 'larger', bytes: 1024 * 1024 });
  else if (field === 'duplicate') Object.assign(node, { type: 'duplicate', is: true });
  else if (field === 'var') Object.assign(node, { type: 'var', name: Object.keys(S.doc.dictionaries)[0] || '', found: true });
  else Object.assign(node, { type: 'date', field, op: 'older_than_days', value: 30 });
}

function renderLeaf(node, parent, index) {
  const f = leafField(node);
  const fieldSel = select(Object.keys(FIELD_LABEL).map((k) => [k, t(FIELD_LABEL[k])]), f, (v) => {
    if (TEXT_FIELDS.includes(v) && node.type === 'text') {
      // În conținut se caută bucăți de text, nu potriviri exacte.
      if (v === 'content' && node.field !== 'content' && (node.op === 'is' || node.op === 'is_not')) node.op = node.op === 'is' ? 'contains' : 'does_not_contain';
      node.field = v;
    } else changeLeafField(node, v);
    markDirty(); render();
  }, t('field'));

  const kids = [fieldSel];
  let vi = null;
  if (node.type === 'text') {
    kids.push(select(TEXT_OPS.map(([v, k]) => [v, t(k)]), node.op, (v) => { node.op = v; markDirty(); render(); }, t('operator')));
    if (node.op !== 'is_nothing') { vi = textInput(node.value, (v) => { node.value = v; markDirty(); }, t('value')); kids.push(vi); }
    else kids.push(h('span', { class: 'grow' }));
  } else if (node.type === 'duplicate') {
    kids.push(select([['true', t('dupIs')], ['false', t('dupIsNot')]], String(node.is !== false), (v) => { node.is = v === 'true'; markDirty(); }, t('operator')));
    kids.push(h('span', { class: 'grow' }));
  } else if (node.type === 'var') {
    const names = Object.keys(S.doc.dictionaries);
    if (node.name && !names.includes(node.name)) names.unshift(node.name);
    const make = () => newVarFromRule((n) => { node.name = n; });
    if (names.length) {
      vi = select([...names.map((n) => [n, n]), [NEW_VAR, t('varNewFromRule')]], node.name,
        (v) => { if (v === NEW_VAR) { render(); make(); return; } node.name = v; markDirty(); render(); }, t('fieldVar'));
      kids.push(vi);
    } else {
      kids.push(h('button', { type: 'button', class: 'btn small', onclick: make }, t('varCreate')));
    }
    kids.push(select([['true', t('varFound')], ['false', t('varNotFound')]], String(node.found !== false), (v) => { node.found = v === 'true'; markDirty(); render(); }, t('operator')));
    kids.push(h('span', { class: 'grow' }));
  } else if (node.type === 'size') {
    kids.push(select([['larger', t('opLarger')], ['smaller', t('opSmaller')]], node.op, (v) => { node.op = v; markDirty(); }, t('operator')));
    if (!sizeUnit.has(node)) sizeUnit.set(node, pickUnit(node.bytes));
    const unit = sizeUnit.get(node);
    kids.push(h('input', { class: 'inp num', type: 'number', min: 0, step: 'any', 'aria-label': t('value'),
      value: node.bytes / unitFactor(unit),
      oninput: (e) => { node.bytes = Math.max(0, Math.round((parseFloat(e.target.value) || 0) * unitFactor(sizeUnit.get(node)))); markDirty(); } }));
    kids.push(select(UNITS.map(([u]) => [u, t('unit' + u)]), unit, (v) => {
      const cur = node.bytes / unitFactor(sizeUnit.get(node));
      sizeUnit.set(node, v);
      node.bytes = Math.round(cur * unitFactor(v));
      markDirty(); render();
    }, t('unit')));
  } else {
    kids.push(select(DATE_OPS.map(([v, k]) => [v, t(k)]), node.op, (v) => {
      node.op = v;
      node.value = v === 'before' || v === 'after' ? Math.floor(Date.now() / 1000) : 30;
      markDirty(); render();
    }, t('operator')));
    if (node.op === 'before' || node.op === 'after') {
      kids.push(h('input', { class: 'inp', type: 'date', value: dateToInput(node.value), 'aria-label': t('value'),
        oninput: (e) => { node.value = inputToDate(e.target.value); markDirty(); } }));
    } else {
      kids.push(h('input', { class: 'inp num', type: 'number', min: 0, step: 1, value: node.value, 'aria-label': t('value'),
        oninput: (e) => { node.value = Math.max(0, parseInt(e.target.value, 10) || 0); markDirty(); } }));
    }
  }
  if (node.type === 'text' && node.field === 'content') kids.push(info(t('fieldContent'), t('contentHelp')));
  if (node.type === 'duplicate') kids.push(info(t('fieldDuplicate'), t('dupHelp')));
  if (node.type === 'var') kids.push(info(t('fieldVar'), t('varCondHelp')));
  kids.push(iconBtn('x', t('removeCondition'), () => { parent.items.splice(index, 1); markDirty(); render(); }));
  return h('div', { class: 'leaf' }, h('div', { class: 'cond-row' }, kids),
    node.type === 'text' ? issueSlot(node, 'value', vi || null) : null,
    node.type === 'var' ? issueSlot(node, 'name', vi || null) : null,
    node.type === 'duplicate' ? dupWithin(node) : null);
}

/// Unde se mai caută originalul unei dubluri, pe lângă folderul fișierului.
function dupWithin(node) {
  const within = node.within || [];
  const add = async () => {
    let path = null;
    try { path = await TAURI.dialog.open({ directory: true, multiple: false, title: t('pickFolder') }); }
    catch (e) { toast(String(e), true); return; }
    if (!path) return;
    node.within = within.filter((w) => w.path !== path).concat([{ path, recursive: true }]);
    markDirty(); render();
  };
  return h('div', { class: 'dup-within' },
    h('div', { class: 'row between' },
      h('span', { class: 'small' }, within.length ? t('dupWithinTitle') : t('dupSameFolderOnly')),
      h('span', { class: 'row' },
        h('button', { type: 'button', class: 'btn small', onclick: add }, icon('folder'), t('dupAddFolder')),
        info(t('dupWithinTitle'), t('dupWithinHelp')))),
    within.map((w, i) => h('div', { class: 'folder-row' },
      textInput(w.path, (v) => { w.path = v; markDirty(); }, t('folderPath'), { class: 'inp mono' }),
      check(t('withSubfolders'), w.recursive !== false, (v) => { w.recursive = v; markDirty(); }),
      iconBtn('x', t('removeFolder'), () => {
        within.splice(i, 1);
        if (!within.length) delete node.within;
        markDirty(); render();
      }))));
}

function renderGroup(node, depth, parent, index) {
  const top = depth === 0;
  const opts = top
    ? [['all', t('groupAll')], ['any', t('groupAny')], ['all_files', t('allFiles')]]
    : [['all', t('groupAll')], ['any', t('groupAny')]];
  const head = h('div', { class: 'cond-row group-head' },
    select(opts, node.type, (v) => {
      if (v === 'all_files') { const r = rule(); r.condition = { type: 'all_files' }; }
      else if (node.type === 'all_files') { node.type = v; node.items = [defaultLeaf()]; }
      else node.type = v;
      markDirty(); render();
    }, t('groupOperator')),
    top ? null : iconBtn('x', t('removeGroup'), () => { parent.items.splice(index, 1); markDirty(); render(); }));
  if (node.type === 'all_files') return h('div', { class: 'group' }, head);
  const body = node.items.map((child, i) =>
    child.type === 'all' || child.type === 'any' ? renderGroup(child, depth + 1, node, i) : renderLeaf(child, node, i));
  const canNest = depth < 7;
  return h('div', { class: top ? 'group top' : 'group nested' }, head, issueSlot(node, 'items', null), body,
    h('div', { class: 'row' },
      h('button', { type: 'button', class: 'btn', onclick: () => { node.items.push(defaultLeaf()); markDirty(); render(); } }, t('addCondition')),
      canNest ? h('button', { type: 'button', class: 'btn', onclick: () => { node.items.push({ type: 'all', items: [defaultLeaf()] }); markDirty(); render(); } }, t('addGroup')) : null));
}

// ---------- acțiuni ----------
const ACTION_TYPES = [['move', 'actMove'], ['copy', 'actCopy'], ['rename', 'actRename'], ['change_extension', 'actChangeExt'],
  ['extract', 'actExtract'], ['trash', 'actTrash'], ['delete', 'actDelete'], ['command', 'actCommand'], ['run', 'actRun'], ['confirm', 'actConfirm']];
const CONFLICTS = () => [['rename', t('conflictRename')], ['dedupe', t('conflictDedupe')], ['skip', t('conflictSkip')], ['overwrite', t('conflictOverwrite')]];
const ACTION_LABEL = Object.fromEntries(ACTION_TYPES);

function renderAction(a, i) {
  const r = rule();
  const set = (k) => (v) => { a[k] = v; markDirty(); };
  const body = [];
  switch (a.type) {
    case 'move':
    case 'copy':
      body.push(field(a, 'dest', h('label', { class: 'lbl' }, t('destFolder'), tplInput(a.dest, set('dest'), t('destFolder'), false, 'dir'))));
      body.push(h('p', { class: 'hint' }, t('destHint')));
      body.push(h('div', { class: 'row wrap' }, h('span', { class: 'muted' }, t('ifExists')),
        select(CONFLICTS(), a.conflict, (v) => { a.conflict = v; markDirty(); }, t('ifExists')), conflictInfo()));
      body.push(h('div', { class: 'row wrap' },
        check(t('keepStructure'), a.keep_structure, (v) => { a.keep_structure = v; markDirty(); }),
        a.type === 'move'
          ? check(t('removeEmpty'), a.remove_empty, (v) => { a.remove_empty = v; markDirty(); })
          : check(t('thenTarget'), a.then_target, (v) => { a.then_target = v; markDirty(); })));
      break;
    case 'rename':
      body.push(field(a, 'name', h('label', { class: 'lbl' }, t('newName'), tplInput(a.name, set('name'), t('newName')))));
      body.push(h('div', { class: 'row wrap' }, h('span', { class: 'muted' }, t('ifExists')),
        select(CONFLICTS(), a.conflict, (v) => { a.conflict = v; markDirty(); }, t('ifExists')), conflictInfo()));
      break;
    case 'extract':
      body.push(field(a, 'dest', h('label', { class: 'lbl' }, t('extractTo'), tplInput(a.dest, set('dest'), t('extractTo'), false, 'dir'))));
      body.push(h('p', { class: 'hint' }, t('extractHint')));
      body.push(h('div', { class: 'row wrap' }, h('span', { class: 'muted' }, t('ifFolderExists')),
        select([['rename', t('conflictNewFolder')], ['skip', t('conflictSkipFolder')]], a.conflict === 'skip' ? 'skip' : 'rename',
          (v) => { a.conflict = v; markDirty(); }, t('ifFolderExists'))));
      break;
    case 'change_extension':
      body.push(field(a, 'ext', h('label', { class: 'lbl' }, t('newExt'), tplInput(a.ext, set('ext'), t('newExt')))));
      break;
    case 'trash':
      body.push(check(t('removeEmpty'), a.remove_empty, (v) => { a.remove_empty = v; markDirty(); }));
      break;
    case 'delete':
      body.push(h('p', { class: 'warn' }, t('deleteWarn')));
      body.push(check(t('removeEmpty'), a.remove_empty, (v) => { a.remove_empty = v; markDirty(); }));
      break;
    case 'confirm':
      body.push(h('p', { class: 'hint' }, t('confirmShort')));
      break;
    case 'command':
      body.push(field(a, 'line', h('label', { class: 'lbl' }, t('commandLine'), tplInput(a.line, set('line'), t('commandLine'), false))));
      if (runsMatchedFile(a.line)) body.push(h('p', { class: 'warn' }, t('runsFileWarn')));
      body.push(h('p', { class: 'hint' }, t('commandHint')));
      break;
    case 'run':
      body.push(field(a, 'program', h('label', { class: 'lbl' }, t('program'), tplInput(a.program, set('program'), t('program'), false, 'file'))));
      body.push(field(a, 'args', h('label', { class: 'lbl' }, t('arguments'),
        tplInput((a.args || []).join('\n'), (v) => { a.args = v === '' ? [] : v.split('\n'); markDirty(); }, t('arguments'), true))));
      if (runsMatchedFile(a.program)) body.push(h('p', { class: 'warn' }, t('runsFileWarn')));
      body.push(h('p', { class: 'hint' }, t('runHint')));
      break;
  }
  const typeSel = select(ACTION_TYPES.map(([v, k]) => [v, t(k)]), a.type, (v) => { r.actions[i] = defaultAction(v); markDirty(); render(); }, t('actionType'));
  return h('div', { class: 'action' },
    h('div', { class: 'row between' },
      h('div', { class: 'row' }, h('span', { class: 'num-badge' }, i + 1), typeSel, actionInfo()),
      h('div', { class: 'row tools' },
        iconBtn('up', t('moveUp'), () => { moveItem(r.actions, i, -1); markDirty(); render(); }, i === 0),
        iconBtn('down', t('moveDown'), () => { moveItem(r.actions, i, 1); markDirty(); render(); }, i === r.actions.length - 1),
        iconBtn('x', t('removeAction'), () => { r.actions.splice(i, 1); markDirty(); render(); }))),
    issueSlot(a, 'type', typeSel),
    body);
}

/// Un câmp (eticheta cu controlul) urmat de mesajul lui de validare.
function field(obj, name, labelEl) {
  const ctl = labelEl.querySelector('.inp');
  return [labelEl, issueSlot(obj, name, ctl)];
}

// ---------- previzualizare ----------
function schedulePreview() {
  clearTimeout(pvTimer);
  pvTimer = setTimeout(runPreview, 700);
}

/// Cât timp se refolosește rezultatul pentru aceeași regulă, neschimbată (ex. după o
/// vizită pe altă filă); „Reîmprospătează” verifică din nou oricând.
const PV_REUSE_MS = 120000;

async function runPreview(force) {
  const r = rule();
  if (!r || S.tab !== 'rules') return;
  const doc = { app: 'filepilot', schema_version: 1, rules: [{ ...clone(r), enabled: false }], dictionaries: S.doc.dictionaries || {} };
  const key = JSON.stringify(doc) + LANG;
  if (force !== true && key === S.pv.key) {
    // Verificarea încă rulează (ai plecat și ai revenit): se urmărește mai departe.
    if (S.pv.loading) { startPvPoll(); fillPreview(); return; }
    if (S.pv.data && Date.now() - S.pv.at < PV_REUSE_MS) { fillPreview(); return; }
  }
  S.pv.key = key;
  const token = ++S.pv.token;
  S.pv.loading = true;
  S.pv.prog = null;
  startPvPoll();
  fillPreview();
  try {
    const data = await jsonCmd('preview', { text: JSON.stringify(doc), lang: LANG });
    if (token !== S.pv.token) return;
    S.pv.data = data; S.pv.error = null; S.pv.at = Date.now();
  } catch (e) {
    if (token !== S.pv.token) return;
    S.pv.data = null; S.pv.error = String(e);
  }
  S.pv.loading = false;
  stopPvPoll();
  fillPreview();
}

// ---------- progresul previzualizării ----------
let pvPoll = null;
function startPvPoll() {
  stopPvPoll();
  pvPoll = setInterval(async () => {
    if (!S.pv.loading || S.tab !== 'rules') { stopPvPoll(); return; }
    try { S.pv.prog = await jsonCmd('preview_progress'); } catch (_) { return; }
    if (S.pv.loading) fillPvProgress();
  }, 250);
}
function stopPvPoll() { if (pvPoll) { clearInterval(pvPoll); pvPoll = null; } }

const fmtN = (n) => Number(n || 0).toLocaleString(LANG === 'en' ? 'en-US' : 'ro-RO');

/// „încă ~35 s”, „încă ~4 min”; nimic până nu se poate estima.
function etaText(done, total, elapsedMs) {
  if (!done || done >= total || elapsedMs < 1000) return '';
  const left = ((total - done) * elapsedMs) / done / 1000;
  if (left < 1) return '';
  return left < 90 ? t('etaSec', { n: Math.ceil(left) }) : t('etaMin', { n: Math.round(left / 60) });
}

/// Bara de progres: fișierele verificate, procentul, potrivirile de până acum, timpul rămas.
function progressNode(done, total, matched, elapsedMs, listing) {
  if (listing) return h('div', { class: 'prog' }, h('div', { class: 'prog-bar indet' }, h('span')),
    h('span', { class: 'muted small' }, t('progListing', { n: fmtN(total) })));
  const pct = total ? Math.min(100, Math.floor((done / total) * 100)) : 0;
  const eta = etaText(done, total, elapsedMs);
  return h('div', { class: 'prog' },
    h('div', { class: 'prog-bar', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': String(total), 'aria-valuenow': String(done) },
      h('span', { style: `width:${pct}%` })),
    h('span', { class: 'small' }, t('progChecking', { d: fmtN(done), t: fmtN(total), p: pct }),
      matched != null ? h('span', { class: 'muted' }, ' · ', t('progMatched', { n: fmtN(matched) })) : null,
      eta ? h('span', { class: 'muted' }, ' · ', eta) : null));
}

function fillPvProgress() {
  const el = document.getElementById('pv-progress');
  if (!el) return;
  const p = S.pv.prog;
  if (!S.pv.loading) { fill(el); return; }
  if (!p || p.gen === 0) { fill(el, h('span', { class: 'muted small' }, t('scanning'))); return; }
  fill(el, progressNode(p.checked, p.total, p.matched, p.elapsed_ms, p.phase === 'list'),
    h('button', { type: 'button', class: 'btn small', title: t('stopPreviewHelp'), onclick: () => invoke('stop_preview').catch(() => {}) }, t('stopPreview')));
}

/// Calea scurtată de la început, ca să se vadă finalul: „…\\2026\\factura.pdf”.
function tailPath(p, max = 70) { return p.length > max ? '…' + p.slice(p.length - max + 1) : p; }

/// Ce ar face regula cu fișierul: [text, clasă, titlu].
function planText(f) {
  if (f.pending) return [t('resultAwaitingYou'), 'wait-text', f.duplicate_of ? t('duplicateOf', { path: f.duplicate_of }) : ''];
  if (f.processed || !f.plan) return [t('resultDone'), 'muted', ''];
  const p = f.plan;
  if (p.error) return [t('resultError', { msg: p.error }), 'bad', p.error];
  if (p.duplicate) return [t('resultDuplicate'), '', f.duplicate_of || ''];
  if (p.awaiting) {
    const before = p.dest ? '→ ' + tailPath(p.dest) + ', ' : '';
    return [before + t('resultAwaiting'), 'wait-text', f.duplicate_of ? t('duplicateOf', { path: f.duplicate_of }) : ''];
  }
  const last = p.ops.length ? p.ops[p.ops.length - 1] : null;
  const ext = p.ops.find((o) => o.kind === 'extract');
  if (ext) return [t(p.gone ? 'resultExtractTrash' : 'resultExtract', { dst: tailPath(ext.dst, 60) }), 'mono', ext.dst];
  if (p.gone) return [last && last.kind === 'delete' ? t('resultDelete') : t('resultTrash'), last && last.kind === 'delete' ? 'bad' : '', ''];
  if (p.dest) return ['→ ' + tailPath(p.dest), 'mono', p.dest];
  const copy = p.ops.find((o) => o.kind === 'copy');
  if (copy) return [t('resultCopy', { dst: tailPath(copy.dst, 60) }), 'mono', copy.dst];
  if (p.skipped) return [t('resultSkipped'), 'muted', ''];
  if (p.ops.some((o) => o.kind === 'run')) return [t('resultRun'), '', ''];
  return [t('resultNone'), 'muted', ''];
}

function fillExample(data) {
  const el = document.getElementById('then-example');
  if (!el) return;
  const f = data && data.files.find((x) => x.plan && !x.plan.error);
  if (!f) { el.textContent = ''; el.hidden = true; return; }
  const [text] = planText(f);
  el.hidden = false;
  el.textContent = t('exampleLine', { name: f.name, result: text });
}

function fillPreview() {
  refreshIssues();
  const box = document.getElementById('pv-body');
  const head = document.getElementById('pv-head');
  if (!box) return;
  const { data, error, loading, filter } = S.pv;
  fill(head,
    h('span', { class: 'row' }, t('matchedFiles'),
      data && !loading ? h('span', { class: 'muted' }, t('matchedOf', { m: fmtN(data.matched), s: fmtN(data.scanned) }),
        data.elapsed_ms >= 1000 ? ' · ' + t('tookSec', { n: (data.elapsed_ms / 1000).toLocaleString(LANG === 'en' ? 'en-US' : 'ro-RO', { maximumFractionDigits: 1 }) }) : '') : null),

    h('span', { class: 'row' },
      loading ? null : iconBtn('refresh', t('refresh'), () => runPreview(true)),
      data && data.files.some((f) => f.processed) ? h('button', { type: 'button', class: 'btn small', disabled: !savedRuleId(),
        title: savedRuleId() ? t('markAllPendingHelp') : t('saveRuleFirst'), onclick: markAllPending }, t('markAllPending')) : null,
      h('span', { class: 'seg' }, [['all', 'filterAll'], ['processed', 'filterProcessed'], ['pending', 'filterPending']].map(([v, k]) =>
        h('button', { type: 'button', class: filter === v ? 'on' : '', onclick: () => { S.pv.filter = v; fillPreview(); } }, t(k))))));
  fillPvProgress();
  box.classList.toggle('stale', !!loading && !!data);
  if (error) { fillExample(null); fill(box, h('p', { class: 'muted pad' }, t('previewError', { msg: error }))); return; }
  if (!data) { fillExample(null); fill(box); return; }
  let files = data.files;
  if (filter === 'processed') files = files.filter((f) => f.processed);
  if (filter === 'pending') files = files.filter((f) => !f.processed);
  files = sortFiles(files, S.pv.sort);
  fillExample(data);
  const rows = files.map((f) => { const [res, cls, title] = planText(f); return h('div', { class: 'frow', oncontextmenu: (ev) => previewMenu(ev, f) },
    h('span', { class: 'ell', title: f.path || f.name }, f.name),
    h('span', { class: 'ell ' + cls, title: title || res }, res),
    h('span', { class: 'muted' }, fmtBytes(f.size)),
    h('span', { class: 'muted' }, fmtTime(f.modified)),
    f.pending ? h('span', { class: 'chip wait' }, t('stateAwaiting'))
      : h('span', { class: f.processed ? 'chip ok' : 'chip wait' }, f.processed ? t('stateProcessed') : t('statePending'))); });
  fill(box,
    h('div', { class: 'frow fhead muted' }, [['name', 'colName'], ['result', 'colResult'], ['size', 'colSize'], ['modified', 'colModified'], ['state', 'colState']]
      .map(([k, label]) => sortHeader(label, k, S.pv.sort, (v) => { S.pv.sort = v; fillPreview(); }))),
    rows.length ? h('div', { class: 'flist' }, rows) : h('p', { class: 'muted pad' }, t('noMatches')),
    data.truncated ? h('p', { class: 'muted pad' }, t('truncated', { n: data.files.length })) : null,
    data.slow ? h('p', { class: 'muted pad' }, t('previewSlow', { n: fmtN(data.scanned), t: fmtN(data.found ?? data.scanned) })) : null,
    data.stopped ? h('div', { class: 'row pad warn-row' }, h('span', {}, t('previewStopped', { n: fmtN(data.scanned), t: fmtN(data.found ?? data.scanned) })),
      h('button', { type: 'button', class: 'btn small', onclick: () => runPreview(true) }, t('previewResume'))) : null,
    data.capped ? h('p', { class: 'muted pad' }, t('previewCapped', { n: fmtN(data.found) })) : null,
    data.unavailable.length ? h('p', { class: 'warn pad' }, t('unavailableFolders', { list: data.unavailable.join(', ') })) : null);
}

// ---------- sortare (ca în Explorer: clic pe coloană, încă un clic inversează) ----------
const collator = () => new Intl.Collator(LANG === 'en' ? 'en' : 'ro', { numeric: true, sensitivity: 'base' });
/// Starea în ordinea în care contează: de confirmat, în așteptare, procesat.
const stateRank = (f) => (f.pending ? 0 : f.processed ? 2 : 1);

function sortFiles(files, sort) {
  if (!sort || !sort.key) return files;
  const c = collator();
  const by = {
    name: (a, b) => c.compare(a.name, b.name),
    result: (a, b) => c.compare(planText(a)[0], planText(b)[0]),
    size: (a, b) => a.size - b.size,
    modified: (a, b) => a.modified - b.modified,
    state: (a, b) => stateRank(a) - stateRank(b),
  }[sort.key];
  if (!by) return files;
  const out = files.slice().sort((a, b) => by(a, b) || c.compare(a.name, b.name));
  return sort.desc ? out.reverse() : out;
}

/// Antet de coloană care sortează; săgeata arată coloana și sensul.
function sortHeader(label, key, sort, onchange) {
  const on = sort && sort.key === key;
  return h('button', { type: 'button', class: 'sort-h' + (on ? ' on' : ''), title: t('sortBy', { col: t(label) }),
    'aria-sort': on ? (sort.desc ? 'descending' : 'ascending') : 'none',
    onclick: () => onchange({ key, desc: on ? !sort.desc : false }) },
  t(label), h('span', { class: 'sort-arrow', 'aria-hidden': 'true' }, on ? (sort.desc ? '▼' : '▲') : ''));
}

/// Id-ul regulii selectate, dacă a fost deja salvată (daemonul o știe după id).
const savedRuleId = () => { const r = rule(); return r && r.id ? String(r.id) : null; };

/// După o cerere către daemon: mesaj, apoi previzualizarea arată starea nouă.
function afterPendingRequest(n) {
  const running = S.status && S.status.daemon_running;
  toast(running ? t('markedPending', { n }) : t('markedPendingLater', { n }));
  setTimeout(() => { if (S.tab === 'rules') runPreview(true); }, 1500);
}

async function markPending(paths) {
  const id = savedRuleId();
  if (!id || !paths.length) return;
  try { await invoke('mark_pending', { rule: id, paths }); afterPendingRequest(paths.length); }
  catch (e) { toast(String(e), true); }
}

async function markAllPending() {
  const id = savedRuleId();
  if (!id) return;
  const r = rule();
  if (!(await confirmModal(t('confirmMarkAll', { name: r.name }), t('markAllPending')))) return;
  try { await invoke('mark_rule_pending', { rule: id }); afterPendingRequest(S.pv.data ? S.pv.data.matched : 0); }
  catch (e) { toast(String(e), true); }
}

function previewMenu(ev, f) {
  const id = savedRuleId();
  contextMenu(ev, [
    { label: id ? t('markPending') : t('markPending') + ' — ' + t('saveRuleFirst'), disabled: !id || !f.processed, run: () => markPending([f.path]) },
    '-',
    { label: t('ctxReveal'), run: () => reveal(f.path) },
    { label: t('ctxCopyPath'), run: () => copyPath(f.path) },
  ]);
}

// ---------- fila Reguli ----------
/// Pentru listă: doar „condiție → acțiuni” (propoziția întreagă e în titlu și în editor).
function ruleSummary(r) {
  const acts = r.actions.length ? r.actions.map(actionWords).join(t('sThen')) : t('sNothing');
  return (r.condition.type === 'all_files' ? t('sAnyFile') : condWords(r.condition)) + ' → ' + acts;
}

// ---------- regula ca propoziție ----------
/// Ultimul folder din cale; dacă e o variabilă (`{modified:AAAA}`), și cel dinainte.
function lastSeg(p) {
  const segs = String(p || '').replace(/[\\/]+$/, '').split(/[\\/]/).filter(Boolean);
  if (!segs.length) return p || '…';
  const last = segs[segs.length - 1];
  return last.includes('{') && segs.length > 1 ? segs[segs.length - 2] + '\\' + last : last;
}
const quote = (v) => (LANG === 'en' ? '“' + v + '”' : '„' + v + '”');
/// „1 zi”, „5 zile”, „30 de zile”.
function days(n) {
  n = Number(n) || 0;
  if (LANG === 'en') return n === 1 ? '1 day' : n + ' days';
  if (n === 1) return '1 zi';
  return n % 100 >= 20 || (n > 0 && n % 100 === 0) ? n + ' de zile' : n + ' zile';
}

/// O condiție în cuvinte; grupurile mari se scurtează („și încă 2 condiții”).
function condWords(n, depth = 0) {
  switch (n.type) {
    case 'all_files': return t('sAllFiles');
    case 'all': case 'any': {
      const parts = n.items.slice(0, 3).map((c) => condWords(c, depth + 1));
      if (!parts.length) return '…';
      let s = parts.join(n.type === 'all' ? t('sAnd') : t('sOr'));
      if (n.items.length > 3) s += t('sMore', { n: n.items.length - 3 });
      return depth > 0 && parts.length > 1 ? '(' + s + ')' : s;
    }
    case 'text': {
      const v = quote(n.value || '…');
      const f = t('sField_' + n.field), fin = t('sIn_' + n.field);
      switch (n.op) {
        case 'is_not': return t('sIsNot', { f, v });
        case 'contains': return t('sContains', { fin, v });
        case 'does_not_contain': return t('sNotContains', { fin, v });
        case 'is_nothing': return t('sEmpty', { f, fin });
        default: return t('sIs', { f, v });
      }
    }
    case 'size': return t(n.op === 'larger' ? 'sLarger' : 'sSmaller', { size: fmtBytes(n.bytes) });
    case 'date': {
      const f = t(n.field === 'created' ? 'sCreated' : 'sModified');
      if (n.op === 'older_than_days') return t('sOlder', { f, n: days(n.value) });
      if (n.op === 'within_days') return t('sWithin', { f, n: days(n.value) });
      return t(n.op === 'before' ? 'sBefore' : 'sAfter', { f, d: fmtDate(n.value, 'ZZ.LL.AAAA') });
    }
    case 'var': return t(n.found === false ? 'sVarNotFound' : 'sVarFound', { v: quote(n.name || '…') });
    case 'duplicate': {
      const w = (n.within || []).filter((x) => x.path);
      if (!w.length) return t(n.is === false ? 'sNotDup' : 'sDup');
      const where = w.slice(0, 2).map((x) => quote(lastSeg(x.path))).join(t('sOr'));
      return t(n.is === false ? 'sNotDupWithin' : 'sDupWithin', { w: where + (w.length > 2 ? t('sOthers') : '') });
    }
    default: return '…';
  }
}

/// Destinația în cuvinte. Un dicționar întreg (`{var:TipFisier}`) devine valorile lui:
/// „Documente”, „Imagini”, „Video”, …
function destWords(dest) {
  const m = /^\{var:([^}]+)\}$/.exec(dest || '');
  const d = m && S.doc.dictionaries && S.doc.dictionaries[m[1]];
  if (d) {
    const vals = [...new Set(d.entries.map((e) => e[1]).concat(d.default ? [d.default] : []).filter(Boolean))];
    if (vals.length) return vals.slice(0, 3).map(quote).join(', ') + (vals.length > 3 ? t('sOthers') : '');
  }
  return quote(dest ? lastSeg(dest) : '…');
}

function actionWords(a) {
  switch (a.type) {
    case 'move': return t('sMove', { d: destWords(a.dest) });
    case 'copy': return t('sCopy', { d: destWords(a.dest) });
    case 'rename': return t('sRename', { n: quote(a.name || '…') });
    case 'change_extension': return t('sChangeExt', { e: a.ext || '…' });
    case 'trash': return t('sTrash');
    case 'delete': return t('sDelete');
    case 'command': return t('sCommand');
    case 'run': return t('sRun', { p: a.program ? lastSeg(a.program) : '…' });
    case 'confirm': return t('sConfirm');
    case 'extract': return a.dest === EXTRACT_HERE ? t('sExtractHere') : t('sExtract', { d: quote(a.dest ? lastSeg(a.dest) : '…') });
    default: return '…';
  }
}

function folderWords(r) {
  if (!r.folders.length) return t('sNoFolder');
  const first = quote(lastSeg(r.folders[0].path)) + (r.folders[0].recursive ? t('sSub') : '');
  return r.folders.length > 1 ? t('sFoldersMore', { f: first, n: r.folders.length - 1 }) : first;
}

/// „Când apare în „Downloads” un fișier cu extensia „pdf” → mută în „Documente”.”
function sentenceText(r) {
  const acts = r.actions.length ? r.actions.map(actionWords).join(t('sThen')) : t('sNothing');
  return t('sentence', { f: folderWords(r), c: condWords(r.condition), a: acts });
}

function sentenceNode(r) {
  return h('p', { id: 'rule-sentence', class: 'sentence' + (r.simulate ? ' sim' : '') },
    r.simulate ? h('span', { class: 'chip sim' }, t('simulateChip')) : null, ' ', sentenceText(r));
}

/// După fiecare modificare: propoziția din editor și rezumatul din listă.
function refreshSentence() {
  const r = rule();
  if (!r) return;
  const el = document.getElementById('rule-sentence');
  if (el) el.replaceWith(sentenceNode(r));
  const li = document.querySelector('.rule-item.sel .summary');
  if (li) { li.textContent = ruleSummary(r); li.title = sentenceText(r); }
}

async function addFolder() {
  const r = rule();
  let path = null;
  try {
    path = await TAURI.dialog.open({ directory: true, multiple: false, title: t('pickFolder') });
  } catch (e) { toast(String(e), true); return; }
  if (!path) return;
  if (!r.folders.some((f) => f.path === path)) r.folders.push({ path, recursive: true });
  markDirty(); render();
}

function renderRulesTab() {
  const r = rule();
  const list = h('aside', { class: 'rules-list' },
    h('div', { class: 'row between pad-h' }, h('strong', {}, t('rules')),
      h('span', { class: 'row' }, gateButton(), h('span', { class: 'muted' }, S.doc.rules.length))),
    S.doc.rules.length === 0 ? h('p', { class: 'muted pad' }, t('noRules')) : null,
    S.doc.rules.map((x, i) => h('div', { class: 'rule-row' + (i === S.sel ? ' sel' : ''), oncontextmenu: (e) => ruleMenu(e, i) },
      h('button', { type: 'button', class: 'rule-item' + (i === S.sel ? ' sel' : ''),
        onclick: () => { S.sel = i; S.pv.data = null; S.pv.filter = 'all'; render(); runPreview(); } },
        h('span', { class: 'row rname-row' }, h('span', { class: 'rname ell' }, x.name || '—'),
          x.hidden ? h('span', { class: 'hidden-mark', title: t('hiddenRule') }, icon('eyeOff')) : null,
          x.simulate ? h('span', { class: 'chip sim', title: t('simulateHelp') }, t('simulateChip')) : null),
        h('span', { class: 'muted small summary', title: sentenceText(x) }, ruleSummary(x)),
        h('span', { class: x.enabled ? 'chip ok' : 'chip off' }, x.enabled ? t('active') : t('stopped'))),
      h('div', { class: 'rule-tools' },
        h('input', { type: 'checkbox', checked: !!x.enabled, title: t('toggleRule'), 'aria-label': t('toggleRule') + ': ' + (x.name || '—'),
          onchange: (e) => { setEnabled(x, e.target.checked); } }),
        iconBtn('up', t('moveUp'), () => { const j = moveItem(S.doc.rules, i, -1); if (S.sel === i) S.sel = j; else if (S.sel === j) S.sel = i; markDirty(); render(); }, i === 0),
        iconBtn('down', t('moveDown'), () => { const j = moveItem(S.doc.rules, i, 1); if (S.sel === i) S.sel = j; else if (S.sel === j) S.sel = i; markDirty(); render(); }, i === S.doc.rules.length - 1)))),
    h('button', { type: 'button', class: 'btn add', onclick: newRuleFromTemplate }, icon('plus'), t('newRule')));

  if (!r) return h('div', { class: 'rules-layout' }, list, h('main', { class: 'editor empty' }, h('p', { class: 'muted' }, t('noRules'))));

  const folders = h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('watchedFolders')),
      h('button', { type: 'button', class: 'btn', onclick: addFolder }, icon('folder'), t('addFolder'))),
    r.folders.length === 0 ? h('div', { class: 'pad' }, issueSlot(r, 'folders', null)) : null,
    r.folders.map((f, i) => { const inp = textInput(f.path, (v) => { f.path = v; markDirty(); }, t('folderPath'), { class: 'inp mono' }); return [
      h('div', { class: 'folder-row' }, inp,
        check(t('withSubfolders'), f.recursive !== false, (v) => { f.recursive = v; markDirty(); }),
        iconBtn('x', t('removeFolder'), () => { r.folders.splice(i, 1); markDirty(); render(); })),
      h('div', { class: 'pad-h slot' }, issueSlot(f, 'path', inp))]; }));

  const cond = h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('ifTitle')), h('span', { class: 'muted' }, t('ifHint'))),
    h('div', { class: 'cbody' }, renderGroup(r.condition, 0, null, 0)));

  const acts = h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('thenTitle'))),
    h('div', { class: 'cbody' },
      h('p', { id: 'then-example', class: 'example', hidden: true }),
      issueSlot(r, 'actions', null),
      r.actions.map(renderAction),
      h('div', { class: 'row' },
        h('button', { type: 'button', class: 'btn', onclick: () => { r.actions.push(defaultAction('move')); markDirty(); render(); } }, t('addAction'))),
      check(t('continueAfter'), r.continue_after_move !== false, (v) => { r.continue_after_move = v; markDirty(); })));

  const preview = h('section', { class: 'card pv' },
    h('div', { class: 'ch', id: 'pv-head' }),
    h('div', { id: 'pv-progress', class: 'pv-progress-row' }),
    h('div', { id: 'pv-body' }));

  const editor = h('main', { class: 'editor' },
    h('div', { class: 'row name-row' },
      h('input', { class: 'inp name', type: 'text', value: r.name, 'aria-label': t('ruleName'),
        oninput: (e) => { r.name = e.target.value; markDirty(); const el = document.querySelector('.rule-item.sel .rname'); if (el) el.textContent = r.name || '—'; } }),
      check(t('active'), r.enabled, (v) => setEnabled(r, v)),
      h('span', { title: S.gate.password ? t('hiddenHelp') : t('hiddenHelpNoPw') },
        check(t('hiddenRule'), !!r.hidden, (v) => { r.hidden = v; markDirty(); render(); })),
      h('span', { class: 'row', style: 'gap:4px' },
        check(t('simulate'), !!r.simulate, (v) => { r.simulate = v; markDirty(); render(); }), info(t('simulate'), t('simulateHelp'))),
      h('button', { type: 'button', class: 'btn', onclick: () => {
        const c = clone(r); delete c.id; c.name = r.name + ' (2)'; S.doc.rules.splice(S.sel + 1, 0, c); S.sel++; markDirty(); render();
      } }, t('duplicate')),
      h('button', { type: 'button', class: 'btn danger', onclick: async () => {
        if (!(await confirmModal(t('confirmDeleteRule', { name: r.name }), t('delete')))) return;
        S.doc.rules.splice(S.sel, 1); S.sel = Math.max(0, S.sel - 1); markDirty(); render();
      } }, t('deleteRule'))),
    sentenceNode(r),
    r.enabled ? null : draftBanner(r),
    folders,
    h('div', { class: 'two' }, cond, acts),
    preview,
    renderJsonCard(r));
  return h('div', { class: 'rules-layout' }, list, editor);
}

// ---------- fila Jurnal ----------
const OP_LABEL = { move: 'opMove', copy: 'opCopy', rename: 'opRename', change_ext: 'opChangeExt', trash: 'opTrash', delete: 'opDelete', run: 'opRun', extract: 'opExtract' };
const ST_LABEL = { done: 'stDone', failed: 'stFailed', undone: 'stUndone', in_progress: 'stInProgress', simulated: 'stSimulated' };
const UNDOABLE = new Set(['move', 'copy', 'rename', 'change_ext', 'trash', 'extract']);

const JPAGE = 100;
const PERIOD_DAYS = { today: 0, '7d': 7, '30d': 30 };
function periodSince(p) {
  if (!(p in PERIOD_DAYS)) return null;
  const d = new Date(); d.setHours(0, 0, 0, 0); d.setDate(d.getDate() - PERIOD_DAYS[p]);
  return Math.floor(d.getTime() / 1000);
}

let jToken = 0;
async function loadJournal() {
  const q = S.jq;
  const token = ++jToken;
  try {
    const data = await jsonCmd('get_journal', {
      limit: S.jlimit, offset: 0, text: q.text.trim() || null, rule: q.rule || null,
      status: q.status || null, since: periodSince(q.period), until: null,
    });
    if (token !== jToken) return; // a venit între timp un răspuns pentru filtre mai noi
    S.journal = data; S.journalError = null;
  } catch (e) { if (token !== jToken) return; S.journalError = String(e); }
  if (S.tab === 'journal') renderJournalBody();
}

/// Un filtru nou începe iar de la prima pagină.
function setJournalFilter(k, v) {
  S.jq[k] = v; S.jlimit = JPAGE; loadJournal();
}
let jSearchTimer = null;

function describeOutcome(o) {
  switch (o.result) {
    case 'restored': return t('restored', { path: o.detail });
    case 'unrecoverable': return t('cannotRestore', { why: o.detail });
    case 'not_undoable': return t('notUndoable');
    case 'needs_confirmation': return t('restoreNeedsConfirm', { why: o.detail });
    default: return o.detail || o.result;
  }
}

async function doUndo(entry) {
  try {
    let o = await jsonCmd('undo_entry', { id: entry.id, force: false });
    if (o.result === 'needs_confirmation') {
      if (!(await confirmModal(t('restoreNeedsConfirm', { why: o.detail }), t('restore')))) return;
      o = await jsonCmd('undo_entry', { id: entry.id, force: true });
    }
    toast(describeOutcome(o), o.result !== 'restored');
  } catch (e) { toast(String(e), true); }
  loadJournal();
}

async function doUndoRun(entry) {
  try {
    const res = await jsonCmd('undo_whole_run', { run: entry.run, force: false });
    const ok = res.filter((o) => o.result === 'restored').length;
    toast(t('restoredRun', { n: ok }), ok === 0);
  } catch (e) { toast(String(e), true); }
  loadJournal();
}

/// Unde e fișierul acum: destinația pentru o operație reușită, altfel sursa.
const entryPath = (e) => (e.status === 'done' && e.dst && e.kind !== 'copy' ? e.dst : e.src);

async function copyPath(p) {
  try { await navigator.clipboard.writeText(p); toast(t('copied')); } catch (_) { toast(t('copyFailed'), true); }
}
async function reveal(p) {
  try { await invoke('reveal', { path: p }); } catch (e) { toast(String(e), true); }
}

/// Where a click on a journal entry takes you: where the file went (a move, rename, copy or
/// extraction that worked), or where it still is. Nothing for a file sent to the Recycle Bin
/// or deleted.
function placeOf(e) {
  if (e.status === 'done' && e.dst) return e.dst;
  if (e.status === 'done' && (e.kind === 'trash' || e.kind === 'delete')) return null;
  return e.src;
}

/// A path with a folder icon, like on the Duplicates page, that shows the file in Explorer,
/// selected (its folder if it is gone).
let lastReveal = { path: null, at: 0 };
const dirOf = (p) => String(p || '').replace(/[\\/][^\\/]*$/, '');
function placeLink(path, text, cls = '', title = null) {
  if (!path) return h('span', { class: ('ell ' + cls).trim(), title: title || '' }, text);
  return h('button', { type: 'button', class: ('path-link ' + cls).trim(), title: title || t('revealHere', { path }),
    onclick: (ev) => {
      ev.stopPropagation();
      // A double click is one request, not two Explorer windows.
      const now = Date.now();
      if (lastReveal.path === path && now - lastReveal.at < 800) return;
      lastReveal = { path, at: now };
      reveal(path);
    },
    ondblclick: (ev) => ev.stopPropagation() }, icon('folder'), h('span', { class: 'ell' }, text));
}

/// "source → destination": a click anywhere shows where the file is now (long paths are cut
/// short, so the whole cell is the link).
function journalFileCell(e) {
  const at = placeOf(e);
  const tip = e.src + (e.dst ? ' → ' + e.dst : '') + (e.message ? '\n' + e.message : '');
  const parts = [e.src, e.dst ? ' → ' + e.dst : ''];
  return placeLink(at, parts, 'mono jfile', at ? tip + '\n\n' + t('revealHere', { path: at }) : tip);
}

function journalMenu(ev, e, runCount) {
  const undoable = e.status === 'done' && UNDOABLE.has(e.kind);
  const here = entryPath(e);
  contextMenu(ev, [
    { label: t('ctxReveal'), run: () => reveal(here) },
    { label: t('ctxCopyPath'), run: () => copyPath(here) },
    e.dst && here !== e.dst ? { label: t('ctxCopyDst'), run: () => copyPath(e.dst) } : null,
    e.message ? { label: t('ctxCopyError'), run: () => copyPath(e.message) } : null,
    '-',
    { label: t('restore'), disabled: !undoable, run: () => doUndo(e) },
    runCount > 1 ? { label: t('restoreRunN', { n: runCount }), disabled: !undoable, run: () => doUndoRun(e) } : null,
    '-',
    e.rule != null ? { label: t('ctxOnlyRule', { name: e.rule }), run: () => { setJournalFilter('rule', e.rule_id); render(); } } : null,
  ]);
}

function renderJournalBody() {
  const box = document.getElementById('journal-body');
  const count = document.getElementById('journal-count');
  if (!box) return;
  if (count) count.textContent = !S.journal ? '' : S.journal.matched === S.journal.total
    ? t('journalTotal', { n: S.journal.total }) : t('journalMatched', { m: S.journal.matched, n: S.journal.total });
  if (S.journalError) { fill(box, h('p', { class: 'warn pad' }, S.journalError)); return; }
  if (!S.journal) { fill(box, h('p', { class: 'muted pad' }, t('scanning'))); return; }
  const es = S.journal.entries;
  const runCounts = {};
  es.forEach((e) => { runCounts[e.run] = (runCounts[e.run] || 0) + 1; });
  const filtered = S.jq.text.trim() || S.jq.rule || S.jq.status || S.jq.period;
  const rows = es.map((e, i) => {
    // Operațiile aceleiași rulări (același fișier, acțiuni succesive) se leagă vizual.
    const cont = i > 0 && es[i - 1].run === e.run;
    const n = runCounts[e.run];
    return h('div', { class: 'jrow' + (cont ? ' cont' : ''), oncontextmenu: (ev) => journalMenu(ev, e, n) },
      h('span', { class: 'muted', title: fmtTime(e.ts) }, cont ? '' : fmtTime(e.ts)),
      h('span', { class: 'ell', title: e.rule ?? '' }, cont ? '' : e.rule ?? t('unknownRule')),
      h('span', {}, t(OP_LABEL[e.kind] || e.kind)),
      journalFileCell(e),
      h('span', { class: 'chip st-' + e.status, title: e.message }, t(ST_LABEL[e.status] || e.status)),
      h('span', { class: 'row' },
        e.status === 'done' && UNDOABLE.has(e.kind)
          ? h('button', { type: 'button', class: 'btn small', onclick: () => doUndo(e) }, icon('undo'), t('restore')) : null,
        e.status === 'done' && n > 1 && UNDOABLE.has(e.kind) && !cont
          ? h('button', { type: 'button', class: 'btn small', title: t('restoreRunN', { n }), onclick: () => doUndoRun(e) }, t('restoreRun')) : null,
        h('button', { type: 'button', class: 'ibtn more', title: t('more'), 'aria-label': t('more'), onclick: (ev) => journalMenu(ev, e, n) }, '⋯')));
  });
  const more = S.journal.matched > es.length;
  fill(box,
    h('div', { class: 'jrow jhead muted' }, ['colTime', 'colRule', 'colOp', 'colFile', 'colState', 'colActions'].map((k) => h('span', {}, t(k)))),
    es.length === 0 ? h('p', { class: 'muted pad' }, filtered ? t('journalNoMatch') : t('journalEmpty')) : rows,
    more ? h('div', { class: 'row pad', style: 'justify-content:center' },
      h('button', { type: 'button', class: 'btn', onclick: () => { S.jlimit += JPAGE; loadJournal(); } },
        t('loadMore', { n: Math.min(JPAGE, S.journal.matched - es.length) })),
      h('span', { class: 'muted small' }, t('shownOf', { m: es.length, n: S.journal.matched }))) : null);
}

function journalFilters() {
  const q = S.jq;
  const rules = S.doc.rules.filter((r) => r.id).map((r) => [String(r.id), r.name]);
  // O regulă ștearsă între timp rămâne în listă cât e selectată.
  if (q.rule && !rules.some(([id]) => id === q.rule)) rules.push([q.rule, t('unknownRule')]);
  const search = textInput(q.text, (v) => {
    q.text = v; clearTimeout(jSearchTimer);
    jSearchTimer = setTimeout(() => { S.jlimit = JPAGE; loadJournal(); }, 300);
  }, t('journalSearch'), { placeholder: t('journalSearch'), type: 'search' });
  search.classList.add('jsearch');
  const reset = q.text || q.rule || q.status || q.period;
  return h('div', { class: 'row wrap jfilters' },
    search,
    select([['', t('allRules')], ...rules], q.rule, (v) => setJournalFilter('rule', v), t('colRule')),
    select([['', t('allStates')], ...Object.entries(ST_LABEL).map(([k, l]) => [k, t(l)])], q.status, (v) => setJournalFilter('status', v), t('colState')),
    select([['', t('periodAll')], ['today', t('periodToday')], ['7d', t('period7')], ['30d', t('period30')]], q.period, (v) => setJournalFilter('period', v), t('period')),
    reset ? h('button', { type: 'button', class: 'btn small', onclick: () => { S.jq = { text: '', rule: '', status: '', period: '' }; S.jlimit = JPAGE; loadJournal(); render(); } }, t('resetFilters')) : null);
}

// ---------- confirmări ----------
/// Pe Jurnal câte 20 pe pagină; pe Acasă doar primele 5, cu legătură spre toate.
const PEND_PAGE = 20;
const PEND_HOME = 5;
const pendKey = (x) => JSON.stringify([x.total, x.offset, x.sort, x.items.map((p) => [p.id, p.exists, p.duplicate_of])]);

let pendToken = 0;
async function loadPending() {
  const home = S.tab !== 'journal';
  const limit = home ? PEND_HOME : PEND_PAGE;
  const offset = home ? 0 : S.pend.page * PEND_PAGE;
  const token = ++pendToken;
  let r;
  const { key: sort, desc } = S.pend.sort;
  try { r = await jsonCmd('get_pending', { limit, offset, sort, desc }); } catch (_) { return; }
  if (token !== pendToken) return;
  // Pagina s-a golit (s-au aprobat cele de pe ea): ultima pagină care mai are ceva.
  if (!r.items.length && r.total && offset > 0) {
    S.pend.page = Math.max(0, Math.ceil(r.total / PEND_PAGE) - 1);
    loadPending();
    return;
  }
  const before = pendKey(S.pend);
  S.pend = { ...S.pend, total: r.total, ids: r.ids, items: r.items, offset: r.offset };
  if (pendKey(S.pend) !== before) {
    fillPendingBadge();
    if (S.tab === 'journal' || S.tab === 'home') fillPending();
  }
}

/// `ids`: cele de răspuns. Mai multe deodată (toate): întâi o confirmare.
async function answerPending(ids, approve, askFirst) {
  if (!ids.length) return;
  if (askFirst && !(await confirmModal(t(approve ? 'approveAllConfirm' : 'rejectAllConfirm', { n: ids.length }), approve ? t('approveAll') : t('rejectAll')))) return;
  try {
    await invoke('answer_pending', { ids, approve });
  } catch (e) { toast(String(e), true); return; }
  const gone = new Set(ids);
  S.pend = { ...S.pend, total: Math.max(0, S.pend.total - ids.length), ids: S.pend.ids.filter((i) => !gone.has(i)),
    items: S.pend.items.filter((p) => !gone.has(p.id)) };
  fillPendingBadge(); fillPending();
  toast(t(approve ? 'approvedN' : 'rejectedN', { n: ids.length }));
  loadPending();
  setTimeout(() => { loadPending(); if (S.tab === 'journal') loadJournal(); if (S.tab === 'home') loadHome(); }, 1200);
}

function fillPendingBadge() {
  const b = document.getElementById('pending-badge');
  if (!b) return;
  b.textContent = S.pend.total ? String(S.pend.total) : '';
  b.hidden = !S.pend.total;
}

function pendingMenu(ev, p) {
  contextMenu(ev, [
    { label: t('approve'), run: () => answerPending([p.id], true) },
    { label: t('reject'), run: () => answerPending([p.id], false) },
    '-',
    { label: t('ctxReveal'), run: () => reveal(p.path) },
    { label: t('ctxCopyPath'), run: () => copyPath(p.path) },
    p.duplicate_of ? { label: t('ctxRevealOriginal'), run: () => reveal(p.duplicate_of) } : null,
  ]);
}

function fillPending() {
  const box = document.getElementById('pending-box');
  if (!box) return;
  const { total, ids, offset } = S.pend;
  if (!total) { box.replaceChildren(); return; }
  const home = S.tab === 'home';
  // Până sosește pagina potrivită filei, nu se arată mai mult decât încape pe ea.
  const items = S.pend.items.slice(0, home ? PEND_HOME : PEND_PAGE);
  const fname = (x) => x.split(/[\\/]/).pop();
  const all = ids.slice();
  const pages = Math.ceil(total / PEND_PAGE);
  const go = (page) => { S.pend.page = page; loadPending(); };
  fill(box, h('section', { class: 'card pending-card' },
    h('div', { class: 'ch' }, h('span', {}, t('pendingTitle', { n: fmtN(total) })),
      h('span', { class: 'row' },
        h('button', { type: 'button', class: 'btn small primary', onclick: () => answerPending(all, true, all.length > 1) },
          all.length > 1 ? t('approveAllN', { n: fmtN(all.length) }) : t('approveAll')),
        h('button', { type: 'button', class: 'btn small', onclick: () => answerPending(all, false, all.length > 1) },
          all.length > 1 ? t('rejectAllN', { n: fmtN(all.length) }) : t('rejectAll')))),
    total > 1 ? h('div', { class: 'row wrap psort' }, h('span', { class: 'muted small' }, t('sortLabel')),
      h('span', { class: 'seg' }, [['ts', 'sortDate'], ['name', 'colName'], ['size', 'colSize'], ['folder', 'colFolder'], ['rule', 'colRule']]
        .map(([k, label]) => sortHeader(label, k, S.pend.sort, (v) => { S.pend.sort = v; S.pend.page = 0; loadPending(); })))) : null,
    items.map((p) => h('div', { class: 'prow', oncontextmenu: (ev) => pendingMenu(ev, p) },
      h('div', { class: 'pmain' },
        h('span', { class: 'row pname' }, h('span', { class: 'ell mono', title: p.path }, fname(p.path)),
          h('span', { class: 'muted small' }, fmtBytes(p.size))),
        h('button', { type: 'button', class: 'path-link small ell mono', title: t('revealHere', { path: p.path }), onclick: () => reveal(p.path) },
          icon('folder'), h('span', { class: 'ell' }, p.folder || p.path)),
        h('span', { class: 'muted small ell' },
          p.rule ?? t('unknownRule'), ' · ', fmtTime(p.ts),
          p.duplicate_of ? [' · ', t('duplicateOfLabel'), ' ',
            h('button', { type: 'button', class: 'path-link inline', title: t('revealHere', { path: p.duplicate_of }), onclick: () => reveal(p.duplicate_of) },
              fname(p.duplicate_of))] : null,
          !p.exists ? [' · ', h('span', { class: 'bad-text' }, t('pendingChanged'))] : null)),
      h('span', { class: 'row pnext' }, p.next.map((k) => h('span', { class: 'chip' }, t(ACTION_LABEL[k] || k)))),
      h('span', { class: 'row' },
        h('button', { type: 'button', class: 'btn small', title: t('revealHere', { path: p.path }), disabled: !p.exists, onclick: () => reveal(p.path) },
          icon('folder'), t('showFile')),
        h('button', { type: 'button', class: 'btn small primary', onclick: () => answerPending([p.id], true) }, t('approve')),
        h('button', { type: 'button', class: 'btn small', onclick: () => answerPending([p.id], false) }, t('reject')),
        h('button', { type: 'button', class: 'ibtn more', title: t('more'), 'aria-label': t('more'), onclick: (ev) => pendingMenu(ev, p) }, '⋯')))),
    home
      ? (total > items.length ? h('div', { class: 'row pad between' },
        h('span', { class: 'muted small' }, t('pendingShown', { m: items.length, n: fmtN(total) })),
        h('button', { type: 'button', class: 'btn small', onclick: () => { S.pend.page = 0; setTab('journal'); } }, t('pendingSeeAll', { n: fmtN(total) }))) : null)
      : (pages > 1 ? h('div', { class: 'row pad pager' },
        h('button', { type: 'button', class: 'btn small', disabled: S.pend.page === 0, onclick: () => go(S.pend.page - 1) }, '‹ ', t('prevPage')),
        h('span', { class: 'muted small' }, t('pageRange', { a: fmtN(offset + 1), b: fmtN(offset + items.length), n: fmtN(total) })),
        h('button', { type: 'button', class: 'btn small', disabled: S.pend.page >= pages - 1, onclick: () => go(S.pend.page + 1) }, t('nextPage'), ' ›')) : null),
    home ? null : h('p', { class: 'hint pad' }, t('pendingHelp'))));
}

function renderJournalTab() {
  return h('div', { class: 'page' },
    h('div', { class: 'row between' }, h('h2', {}, t('journal')),
      h('div', { class: 'row' }, h('span', { id: 'journal-count', class: 'muted' }),
        h('button', { type: 'button', class: 'btn', onclick: () => { loadJournal(); loadPending(); } }, icon('refresh'), t('refresh')))),
    h('div', { id: 'pending-box' }),
    journalFilters(),
    h('section', { class: 'card' }, h('div', { id: 'journal-body' })));
}

// ---------- fila Acasă ----------
/// „1 fișier”, „5 fișiere”, „20 de fișiere” (în engleză doar singular/plural).
function plural(n, one, few, many) {
  if (n === 1) return t(one, { n });
  return t(LANG !== 'en' && (n % 100 >= 20 || (n > 0 && n % 100 === 0)) ? many : few, { n });
}
const nFiles = (n) => plural(n, 'file1', 'fileFew', 'fileMany');
const nErrors = (n) => plural(n, 'err1', 'errFew', 'errMany');
const fname = (p) => String(p || '').split(/[\\/]/).pop();

/// „acum 5 min”, „azi, 14:30”, „ieri, 09:12”, altfel data completă.
function relTime(ts) {
  const diff = Date.now() / 1000 - ts;
  if (diff < 60) return t('relNow');
  if (diff < 3600) return t('relMin', { n: Math.floor(diff / 60) });
  const d = new Date(ts * 1000);
  const hm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const day = new Date(); day.setHours(0, 0, 0, 0);
  if (d >= day) return t('relToday', { t: hm });
  day.setDate(day.getDate() - 1);
  if (d >= day) return t('relYesterday', { t: hm });
  return fmtTime(ts);
}

/// Începuturile ultimelor 7 zile (ora locală), plus începutul zilei de mâine.
function weekBounds() {
  const d = new Date(); d.setHours(0, 0, 0, 0); d.setDate(d.getDate() - 6);
  const out = [];
  for (let i = 0; i < 8; i++) { out.push(Math.floor(d.getTime() / 1000)); d.setDate(d.getDate() + 1); }
  return out;
}

let hToken = 0;
async function loadHome() {
  const token = ++hToken;
  const bounds = weekBounds();
  const [stats, recent] = await Promise.all([
    jsonCmd('get_stats', { bounds }).catch((e) => ({ error: String(e) })),
    jsonCmd('get_journal', { limit: 8, offset: 0, text: null, rule: null, status: null, since: null, until: null })
      .catch((e) => ({ error: String(e) })),
  ]);
  if (token !== hToken) return;
  S.home = { stats, recent, bounds };
  if (S.tab === 'home') { fillHomeStatus(); fillHomeWeek(); fillHomeRecent(); fillHomeChecks(); }
}

/// Jurnalul, deja filtrat (din Acasă: erorile de azi, simulările...).
function openJournal(filters) {
  S.jq = { text: '', rule: '', status: '', period: '', ...filters };
  S.jlimit = JPAGE;
  S.journal = null;
  setTab('journal');
}

function openRule(i) {
  S.sel = i; S.pv.data = null; S.pv.filter = 'all';
  setTab('rules');
}

const todayStats = () => {
  const st = S.home && S.home.stats;
  return st && st.days ? st.days[st.days.length - 1] : null;
};

function fillHomeStatus() {
  const box = document.getElementById('home-status');
  if (!box) return;
  const st = S.status;
  const running = !!(st && st.daemon_running);
  const d = (st && st.daemon) || {};
  const paused = running && st.paused_until != null;
  const [cls] = daemonState();
  const title = !running ? t('homeOff') : paused ? t('homePaused') : d.last_error ? t('homeErrorTitle') : t('homeRunning');
  // Cifrele daemonului (regulile încărcate efectiv); fără daemon, cele din fereastră.
  const active = running && d.active != null ? d.active : S.doc.rules.filter((r) => r.enabled).length;
  const total = running && d.rules != null ? d.rules : S.doc.rules.length;
  const folders = running && d.folders ? d.folders.length
    : new Set(S.doc.rules.filter((r) => r.enabled).flatMap((r) => r.folders.map((f) => f.path.toLowerCase()))).size;
  const today = todayStats();
  const sc = running && !paused && d.scan && d.scan.total ? d.scan : null;
  const lines = !running ? [t('homeOffBody')] : paused ? [pauseText(st.paused_until)]
    : [t('homeSub', { a: active, n: total, f: folders }),
      today ? t('homeTodayLine', { files: nFiles(today.done), errors: nErrors(today.failed) }) : null];
  fill(box, h('section', { class: 'card hero' },
    h('div', { class: 'hero-main' },
      h('span', { class: 'dot ' + cls, 'aria-hidden': 'true' }),
      h('div', { class: 'hero-text' },
        h('strong', { class: 'hero-title' }, title),
        lines.filter(Boolean).map((l) => h('span', { class: 'muted' }, l)),
        sc ? h('div', { class: 'hero-scan' }, h('span', { class: 'small' }, t('homeScanning')),
          progressNode(sc.done, sc.total, null, Math.max(0, Date.now() / 1000 - sc.started) * 1000, false)) : null),
      h('div', { class: 'row wrap hero-actions' },
        !running ? h('button', { type: 'button', class: 'btn primary', onclick: startDaemon }, t('startDaemon'))
          : paused ? h('button', { type: 'button', class: 'btn primary', onclick: () => setPause(null) }, t('resume'))
            : [h('button', { type: 'button', class: 'btn', onclick: () => pauseFor(3600) }, t('pause1h')),
              h('button', { type: 'button', class: 'btn', onclick: () => setPause(0) }, t('pauseInf'))]))));
}

function fillHomeWeek() {
  const box = document.getElementById('home-week');
  if (!box) return;
  const st = S.home && S.home.stats;
  if (!st) { fill(box, h('p', { class: 'muted pad' }, t('loadingHome'))); return; }
  if (st.error) { fill(box, h('p', { class: 'warn pad' }, st.error)); return; }
  const days = st.days;
  const sum = (k) => days.reduce((a, x) => a + x[k], 0);
  const max = Math.max(1, ...days.map((x) => x.done + x.failed + x.simulated));
  const loc = LANG === 'en' ? 'en-GB' : 'ro-RO';
  const seg = (cls, v) => (v ? h('span', { class: 'wseg ' + cls, style: `height:${(v / max) * 100}%` }) : null);
  const bars = days.map((x, i) => {
    const date = new Date(S.home.bounds[i] * 1000);
    const tot = x.done + x.failed + x.simulated;
    return h('div', { class: 'wbar' + (i === days.length - 1 ? ' today' : ''),
      title: t('barTip', { day: date.toLocaleDateString(loc, { weekday: 'long', day: 'numeric', month: 'long' }), done: x.done, failed: x.failed, sim: x.simulated }) },
      h('span', { class: 'wnum small' }, tot ? String(tot) : ''),
      h('div', { class: 'wstack' }, seg('s-done', x.done), seg('s-sim', x.simulated), seg('s-failed', x.failed)),
      h('span', { class: 'wlbl small' }, date.toLocaleDateString(loc, { weekday: 'short' }).replace('.', '')));
  });
  const done = sum('done'), failed = sum('failed'), sim = sum('simulated'), undone = sum('undone');
  const any = done + failed + sim + undone > 0;
  const chip = (cls, text, onclick) => (onclick
    ? h('button', { type: 'button', class: 'chip ' + cls, onclick }, text) : h('span', { class: 'chip ' + cls }, text));
  fill(box,
    h('div', { class: 'wchart' }, bars),
    any ? h('div', { class: 'row wrap wtot' },
      chip('ok', t('weekDone', { files: nFiles(done) })),
      failed ? chip('bad', t('weekFailed', { n: failed }), () => openJournal({ status: 'failed', period: '7d' })) : null,
      sim ? chip('sim', t('weekSim', { n: sim }), () => openJournal({ status: 'simulated', period: '7d' })) : null,
      undone ? chip('st-undone', t('weekUndone', { n: undone }), () => openJournal({ status: 'undone', period: '7d' })) : null)
      : h('p', { class: 'muted pad' }, t('weekEmpty')),
    st.top && st.top.length ? [
      h('div', { class: 'sub-head muted small' }, t('topRules')),
      st.top.map((x) => h('button', { type: 'button', class: 'top-row', onclick: () => openJournal({ rule: x.rule_id, period: '7d' }) },
        h('span', { class: 'ell' }, x.rule ?? t('unknownRule')), h('span', { class: 'muted small' }, nFiles(x.files))))] : null);
}

async function undoFromHome(e) {
  await doUndo(e);
  loadHome();
}

function fillHomeRecent() {
  const box = document.getElementById('home-recent');
  if (!box) return;
  const r = S.home && S.home.recent;
  if (!r) { fill(box, h('p', { class: 'muted pad' }, t('loadingHome'))); return; }
  if (r.error) { fill(box, h('p', { class: 'warn pad' }, r.error)); return; }
  if (!r.entries.length) { fill(box, h('p', { class: 'muted pad' }, t('recentEmpty'))); return; }
  fill(box, r.entries.map((e) => {
    const undoable = e.status === 'done' && UNDOABLE.has(e.kind);
    return h('div', { class: 'hrow', oncontextmenu: (ev) => journalMenu(ev, e, 1) },
      h('span', { class: 'muted small', title: fmtTime(e.ts) }, relTime(e.ts)),
      h('div', { class: 'pmain' },
        h('span', { class: 'ell', title: placeOf(e) || e.src }, fname(placeOf(e) || e.src)),
        h('span', { class: 'row hplace' },
          placeOf(e) ? placeLink(placeOf(e), dirOf(placeOf(e)), 'small mono') : null,
          h('span', { class: 'muted small nowrap', title: (e.dst || '') + (e.message ? '\n' + e.message : '') },
            (placeOf(e) ? '· ' : '') + t(OP_LABEL[e.kind] || e.kind) + ' · ' + (e.rule ?? t('unknownRule'))))),
      h('span', { class: 'chip st-' + e.status, title: e.message || '' }, t(ST_LABEL[e.status] || e.status)),
      undoable ? h('button', { type: 'button', class: 'btn small', title: t('restore'), onclick: () => undoFromHome(e) }, icon('undo'), t('restore'))
        : h('span'));
  }));
}

/// Ce cere atenție: foldere indisponibile, ultima eroare, erorile de azi, reguli active cu probleme.
function fillHomeChecks() {
  const box = document.getElementById('home-checks');
  if (!box) return;
  const items = [];
  const st = S.status;
  const d = (st && st.daemon_running && st.daemon) || {};
  if (d.unavailable && d.unavailable.length) items.push(h('div', { class: 'check-row' }, h('span', { class: 'wait-text' }, t('homeUnavailable', { list: d.unavailable.join(', ') }))));
  if (d.last_error) items.push(h('div', { class: 'check-row' }, h('span', { class: 'bad-text ell', title: d.last_error }, t('lastError', { msg: d.last_error }))));
  const today = todayStats();
  if (today && today.failed) {
    items.push(h('div', { class: 'check-row' }, h('span', { class: 'bad-text' }, t('failedTodayN', { n: today.failed })),
      h('button', { type: 'button', class: 'btn small', onclick: () => openJournal({ status: 'failed', period: 'today' }) }, t('seeErrors'))));
  }
  S.doc.rules.forEach((r, i) => {
    if (!r.enabled) return;
    const e = firstError(r);
    if (e) items.push(h('div', { class: 'check-row' }, h('span', { class: 'bad-text ell' }, t('ruleProblem', { name: r.name || '—', msg: e })),
      h('button', { type: 'button', class: 'btn small', onclick: () => openRule(i) }, t('fixRule'))));
  });
  fill(box, items.length ? h('section', { class: 'card checks' },
    h('div', { class: 'ch' }, h('span', {}, t('toCheck'))), items) : null);
}

async function homeToggle(r, on, el) {
  const clean = !S.dirty;
  if (!setEnabled(r, on)) { el.checked = !on; return; }
  if (clean) await save();
}

async function stopSimulation() {
  if (S.dirty) { toast(t('saveFirstHome'), true); return; }
  const sims = S.doc.rules.filter((r) => r.simulate && r.enabled);
  if (!sims.length) return;
  if (!(await confirmModal(t('stopSimConfirm', { n: sims.length }), t('stopSim')))) return;
  sims.forEach((r) => { r.simulate = false; });
  markDirty();
  await save();
  if (!S.dirty) toast(t('simStopped'));
}

function simBanner() {
  const sims = S.doc.rules.filter((r) => r.simulate && r.enabled);
  if (!sims.length) return null;
  return h('section', { class: 'card sim-banner' },
    h('div', { class: 'cbody row wrap between' },
      h('span', {}, h('span', { class: 'chip sim' }, t('simulateChip')), ' ',
        sims.length === 1 ? t('simBanner1') : t('simBannerN', { n: sims.length })),
      h('span', { class: 'row' },
        h('button', { type: 'button', class: 'btn small', onclick: () => openJournal({ status: 'simulated' }) }, t('seeSimulated')),
        h('button', { type: 'button', class: 'btn small primary', onclick: stopSimulation }, t('stopSim')))));
}

function homeRulesCard() {
  const rules = S.doc.rules;
  return h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', { class: 'row' }, t('homeRules'), h('span', { class: 'muted' }, String(rules.length))),
      h('span', { class: 'row' }, gateButton(),
        rules.length ? h('button', { type: 'button', class: 'btn small', onclick: () => setTab('rules') }, t('editRules')) : null)),
    rules.length === 0 ? h('p', { class: 'muted pad' }, t('homeNoRules')) : null,
    rules.map((x, i) => h('div', { class: 'hrule', oncontextmenu: (e) => ruleMenu(e, i) },
      h('label', { class: 'switch', title: t('toggleRule') },
        h('input', { type: 'checkbox', checked: !!x.enabled, 'aria-label': t('toggleRule') + ': ' + (x.name || '—'),
          onchange: (e) => homeToggle(x, e.target.checked, e.target) }),
        h('span', { class: 'slider', 'aria-hidden': 'true' })),
      h('button', { type: 'button', class: 'hrule-main', onclick: () => openRule(i) },
        h('span', { class: 'row rname-row' }, h('span', { class: 'rname ell' }, x.name || '—'),
          x.hidden ? h('span', { class: 'hidden-mark', title: t('hiddenRule') }, icon('eyeOff')) : null,
          x.simulate ? h('span', { class: 'chip sim', title: t('simulateHelp') }, t('simulateChip')) : null),
        h('span', { class: 'muted small summary', title: sentenceText(x) }, ruleSummary(x))))),
    h('div', { class: 'row list-tools hrule-tools' },
      h('button', { type: 'button', class: 'btn small', onclick: async () => { setTab('rules'); await newRuleFromTemplate(); } }, icon('plus'), t('newRule')),
      h('button', { type: 'button', class: 'btn small', onclick: firstRunWizard }, t('wizardAgain'))));
}

function welcomeCard() {
  return h('section', { class: 'card welcome' },
    h('div', { class: 'cbody' },
      h('h2', {}, t('welcomeTitle')),
      h('p', {}, t('welcomeBody')),
      h('div', { class: 'row wrap' },
        h('button', { type: 'button', class: 'btn primary', onclick: firstRunWizard }, t('startWizard')),
        h('button', { type: 'button', class: 'btn', onclick: async () => { setTab('rules'); await newRuleFromTemplate(); } }, t('pickTemplate')))));
}

/// Fără reguli vizibile și fără reguli ascunse blocate: e o instalare nouă.
const looksNew = () => S.doc.rules.length === 0 && !(S.gate.password && !S.gate.unlocked && S.gate.hidden > 0);

function renderHomeTab() {
  return h('div', { class: 'page home-page' },
    h('div', { id: 'home-status' }),
    simBanner(),
    looksNew() ? welcomeCard() : null,
    h('div', { id: 'home-checks' }),
    h('div', { id: 'pending-box' }),
    h('div', { class: 'home-grid' },
      h('div', { class: 'col' },
        h('section', { class: 'card' },
          h('div', { class: 'ch' }, h('span', {}, t('recent')),
            h('button', { type: 'button', class: 'btn small', onclick: () => openJournal({}) }, t('allJournal'))),
          h('div', { id: 'home-recent' }))),
      h('div', { class: 'col' },
        h('section', { class: 'card' }, h('div', { class: 'ch' }, h('span', {}, t('week'))), h('div', { id: 'home-week' })),
        homeRulesCard())));
}

// ---------- ghidul de la prima pornire ----------
const WIZ_PICKS = ['sort', 'dupes', 'extract', 'old', 'invoices', 'cleanup'];
/// Regulile speciale înaintea sortării generale (care ar lua orice fișier).
const WIZ_ORDER = ['cleanup', 'dupes', 'extract', 'invoices', 'old', 'sort'];

function markOnboarded() { try { localStorage.setItem('onboarded', '1'); } catch (_) { /* fără stocare */ } }
function onboarded() { try { return localStorage.getItem('onboarded') === '1'; } catch (_) { return true; } }

async function firstRunWizard() {
  const dictBefore = JSON.stringify(S.doc.dictionaries);
  const w = { step: 0, folder: await downloadsDir(), picks: new Set(['sort', 'dupes']), simulate: true,
    autostart: true, counts: {}, token: 0 };
  const N = 4;
  const root = document.getElementById('modal-root');
  /// Regulile alese, în ordinea în care vor rula. Șablonul de sortare își adaugă dicționarul.
  const build = () => WIZ_ORDER.filter((k) => w.picks.has(k)).map((k) => {
    const r = TEMPLATES.find((x) => x.key === k).make(w.folder);
    r.folders = w.folder ? [{ path: w.folder, recursive: false }] : [];
    r.simulate = w.simulate;
    return [k, r];
  });
  const canNext = () => (w.step === 1 ? !!w.folder.trim() : w.step === 2 ? w.picks.size > 0 : true);
  const syncNext = () => { const b = document.getElementById('wiz-next'); if (b) b.disabled = !canNext(); };

  return new Promise((resolve) => {
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); finish(false); } };
    window.addEventListener('keydown', onKey, true);
    const close = () => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); };

    async function finish(create) {
      close();
      markOnboarded();
      if (!create) { S.doc.dictionaries = JSON.parse(dictBefore); render(); resolve(false); return; }
      w.folder = w.folder.trim();
      const made = build().map(([, r]) => r);
      for (const r of made) {
        r.name = uniqueRuleName(r.name);
        r.enabled = !firstError(r);
        S.doc.rules.push(r);
      }
      S.sel = S.doc.rules.length - made.length;
      if (w.autostart !== !!S.autostart) await setAutostart(w.autostart);
      markDirty();
      await save();
      if (S.status && !S.status.daemon_running) await startDaemon();
      S.tab = 'home';
      render();
      loadHome();
      if (!S.dirty) toast(w.simulate ? t('wizDoneSim', { n: made.length }) : t('wizDone', { n: made.length }));
      resolve(true);
    }

    async function countMatches() {
      const token = ++w.token;
      for (const [k, r] of build()) {
        const doc = { app: 'filepilot', schema_version: 1, rules: [{ ...r, enabled: false }], dictionaries: S.doc.dictionaries || {} };
        let text;
        try {
          const data = await jsonCmd('preview', { text: JSON.stringify(doc), lang: LANG });
          text = data.matched ? t('wizMatches', { files: nFiles(data.matched) }) : t('wizMatchesNone');
        } catch (e) { text = t('previewError', { msg: e }); }
        if (token !== w.token) return;
        w.counts[k] = text;
        const el = document.getElementById('wiz-count-' + k);
        if (el) el.textContent = text;
      }
    }

    const stepWelcome = () => [
      h('h2', {}, t('welcomeTitle')),
      h('ul', { class: 'wiz-list' }, ['wizP1', 'wizP2', 'wizP3', 'wizP4'].map((k) => h('li', {}, t(k)))),
      h('label', { class: 'lbl' }, t('language'),
        select([['ro', 'Română'], ['en', 'English']], LANG, (v) => {
          setLang(v);
          try { localStorage.setItem('lang', v); } catch (_) { /* fără stocare */ }
          invoke('set_language', { lang: LANG }).catch(() => {});
          render(); draw();
        }, t('language')))];

    const stepFolder = () => {
      const inp = textInput(w.folder, (v) => { w.folder = v; syncNext(); }, t('folderPath'), { class: 'inp mono', id: 'wiz-folder' });
      return [
        h('h2', {}, t('wiz2Title')),
        h('div', { class: 'row' }, inp,
          h('button', { type: 'button', class: 'btn', onclick: async () => {
            let p = null;
            try { p = await TAURI.dialog.open({ directory: true, multiple: false, title: t('pickFolder'), defaultPath: w.folder || undefined }); }
            catch (e) { toast(String(e), true); }
            if (p) { w.folder = p; draw(); }
          } }, icon('folder'), t('browseFolder'))),
        h('p', { class: 'hint' }, t('wiz2Help'))];
    };

    const stepWhat = () => [
      h('h2', {}, t('wiz3Title')),
      h('p', { class: 'hint' }, t('wiz3Help')),
      h('div', { class: 'tpl-grid' }, WIZ_PICKS.map((k) => h('label', { class: 'tpl-card pick' + (w.picks.has(k) ? ' on' : '') },
        h('input', { type: 'checkbox', checked: w.picks.has(k), onchange: (e) => {
          if (e.target.checked) w.picks.add(k); else w.picks.delete(k);
          e.target.closest('.tpl-card').classList.toggle('on', e.target.checked);
          syncNext();
        } }),
        h('span', { class: 'pick-text' }, h('strong', {}, t(TPL_TEXT[k][0])), h('span', { class: 'muted small' }, t(TPL_TEXT[k][1])))))),
      h('p', { class: 'hint' }, t('wizOrder'))];

    const stepReview = () => {
      const rules = build();
      return [
        h('h2', {}, t('wiz4Title')),
        h('div', { class: 'wiz-rules' }, rules.map(([k, r]) => h('div', { class: 'wiz-rule' },
          h('strong', {}, r.name),
          h('span', {}, sentenceText(r)),
          h('span', { id: 'wiz-count-' + k, class: 'muted small' }, w.counts[k] || t('scanning'))))),
        h('div', { class: 'wiz-opts' },
          check(t('wizSimulate'), w.simulate, (v) => { w.simulate = v; draw(); }),
          h('p', { class: w.simulate ? 'hint' : 'warn' }, w.simulate ? t('wizSimHelp') : t('wizRealWarn')),
          check(t('autostart'), w.autostart, (v) => { w.autostart = v; }),
          h('p', { class: 'hint' }, t('autostartHelp')))];
    };

    function draw() {
      const steps = [stepWelcome, stepFolder, stepWhat, stepReview];
      const last = w.step === N - 1;
      fill(root, h('div', { class: 'overlay' },
        h('div', { class: 'modal wide wizard', role: 'dialog', 'aria-modal': 'true', 'aria-label': t('wizTitle') },
          h('div', { class: 'row between' },
            h('span', { class: 'muted small' }, t('wizTitle'), ' · ', t('wizStep', { i: w.step + 1, n: N })),
            h('div', { class: 'wiz-dots', 'aria-hidden': 'true' }, Array.from({ length: N }, (_, i) => h('span', { class: i <= w.step ? 'on' : '' })))),
          h('div', { class: 'wiz-body' }, steps[w.step]()),
          h('div', { class: 'row between' },
            h('button', { type: 'button', class: 'btn', onclick: () => finish(false) }, t('wizSkip')),
            h('span', { class: 'row' },
              w.step > 0 ? h('button', { type: 'button', class: 'btn', onclick: () => { w.step--; draw(); } }, t('wizBack')) : null,
              last ? h('button', { id: 'wiz-next', type: 'button', class: 'btn primary', onclick: () => finish(true) }, t('wizCreate'))
                : h('button', { id: 'wiz-next', type: 'button', class: 'btn primary', disabled: !canNext(),
                  onclick: () => { if (!canNext()) return; w.step++; if (w.step === N - 1) { w.counts = {}; countMatches(); } draw(); } }, t('wizNext')))))));
      const focus = root.querySelector(w.step === 1 ? '#wiz-folder' : '#wiz-next');
      if (focus) focus.focus();
    }
    draw();
  });
}

// ---------- fila Dubluri ----------
const DUP_MINS = [[0, 'dupMinAll'], [102400, 'dupMin100k'], [1048576, 'dupMin1m'], [10485760, 'dupMin10m'], [104857600, 'dupMin100m']];
const DUP_PAGE = 50;

/// Folderele implicite: ale regulilor active, plus unde își caută ele originalul.
function dupDefaultFolders() {
  const out = [];
  const add = (f) => { if (f.path && !out.some((x) => x.path.toLowerCase() === f.path.toLowerCase())) out.push({ path: f.path, recursive: f.recursive !== false }); };
  const walk = (c) => {
    if (!c) return;
    if (c.type === 'duplicate') (c.within || []).forEach(add);
    (c.items || []).forEach(walk);
  };
  S.doc.rules.filter((r) => r.enabled).forEach((r) => { r.folders.forEach(add); walk(r.condition); });
  return out;
}

function dupState() {
  if (!S.dups) {
    let saved = null;
    try { saved = JSON.parse(localStorage.getItem('dupSearch') || 'null'); } catch (_) { /* fără stocare */ }
    S.dups = { folders: saved?.folders || dupDefaultFolders(), min: saved?.min ?? 1048576,
      data: null, loading: false, prog: null, error: null, keep: {}, drop: new Set(), shown: DUP_PAGE, token: 0 };
  }
  return S.dups;
}

function saveDupSearch() {
  const d = dupState();
  try { localStorage.setItem('dupSearch', JSON.stringify({ folders: d.folders, min: d.min })); } catch (_) { /* fără stocare */ }
}

/// Implicit: rămâne originalul (cel mai vechi), restul se bifează pentru Coș.
function dupDefaults(data) {
  const d = dupState();
  d.keep = {};
  d.drop = new Set();
  data.groups.forEach((g, i) => {
    d.keep[i] = g.files[0].path;
    g.files.slice(1).forEach((f) => d.drop.add(f.path));
  });
}

let dupPoll = null;
async function runDupSearch() {
  const d = dupState();
  const folders = d.folders.filter((f) => f.path.trim());
  if (!folders.length) { toast(t('dupNoFolders'), true); return; }
  saveDupSearch();
  const token = ++d.token;
  d.loading = true; d.prog = null; d.error = null;
  if (dupPoll) clearInterval(dupPoll);
  dupPoll = setInterval(async () => {
    if (!d.loading) { clearInterval(dupPoll); dupPoll = null; return; }
    try { d.prog = await jsonCmd('dup_progress'); } catch (_) { return; }
    if (S.tab === 'dups') fillDupProgress();
  }, 250);
  render();
  try {
    const data = await jsonCmd('find_duplicates', { text: JSON.stringify({ folders, min_size: d.min }) });
    if (token !== d.token) return;
    d.data = data; d.shown = DUP_PAGE;
    dupDefaults(data);
  } catch (e) {
    if (token !== d.token) return;
    d.error = String(e);
  }
  d.loading = false;
  if (S.tab === 'dups') render();
}

function fillDupProgress() {
  const el = document.getElementById('dup-progress');
  const d = dupState();
  if (!el) return;
  const p = d.prog;
  if (!d.loading) { fill(el); return; }
  const stop = h('button', { type: 'button', class: 'btn small', onclick: () => invoke('stop_dups').catch(() => {}) }, t('stopPreview'));
  if (!p || p.gen === 0 || p.phase === 'list') {
    fill(el, h('div', { class: 'prog' }, h('div', { class: 'prog-bar indet' }, h('span')),
      h('span', { class: 'muted small' }, t('dupListing', { n: fmtN(p ? p.checked : 0) }))), stop);
    return;
  }
  const pct = p.total ? Math.min(100, Math.floor((p.checked / p.total) * 100)) : 0;
  const eta = etaText(p.checked, p.total, p.elapsed_ms);
  fill(el, h('div', { class: 'prog' },
    h('div', { class: 'prog-bar' }, h('span', { style: `width:${pct}%` })),
    h('span', { class: 'small' }, t(p.phase === 'quick' ? 'dupQuick' : 'dupFull', { d: fmtN(p.checked), t: fmtN(p.total), p: pct }),
      eta ? h('span', { class: 'muted' }, ' · ', eta) : null)), stop);
}

/// Ce s-ar trimite în Coș: pe grupuri, cu fișierul păstrat.
function dupSelection() {
  const d = dupState();
  const items = [];
  let n = 0, bytes = 0;
  (d.data?.groups || []).forEach((g, i) => {
    const remove = g.files.map((f) => f.path).filter((p) => p !== d.keep[i] && d.drop.has(p));
    if (remove.length) { items.push({ keep: d.keep[i], remove }); n += remove.length; bytes += remove.length * g.size; }
  });
  return { items, n, bytes };
}

async function trashDupSelection() {
  const { items, n, bytes } = dupSelection();
  if (!n) return;
  if (!(await confirmModal(t('dupTrashConfirm', { n: fmtN(n), size: fmtBytes(bytes) }), t('dupTrash')))) return;
  let res;
  try { res = await jsonCmd('trash_duplicates', { text: JSON.stringify(items) }); }
  catch (e) { toast(String(e), true); return; }
  const d = dupState();
  const left = new Set([...res.skipped, ...res.failed.map((f) => f.path)]);
  const gone = new Set(items.flatMap((x) => x.remove).filter((p) => !left.has(p)));
  // Grupurile rămase (cu cel puțin două fișiere) își păstrează alegerile.
  const keepByPath = new Map(d.data.groups.map((g, i) => [g.files[0].path, d.keep[i]]));
  const groups = d.data.groups.map((g) => ({ ...g, files: g.files.filter((f) => !gone.has(f.path)) })).filter((g) => g.files.length > 1);
  const freedCount = gone.size;
  d.data = { ...d.data, groups, total_groups: Math.max(0, d.data.total_groups - (d.data.groups.length - groups.length)),
    copies: Math.max(0, d.data.copies - freedCount), wasted: Math.max(0, d.data.wasted - res.freed) };
  d.keep = {};
  groups.forEach((g, i) => { const k = keepByPath.get(g.files[0].path); d.keep[i] = g.files.some((f) => f.path === k) ? k : g.files[0].path; });
  gone.forEach((p) => d.drop.delete(p));
  render();
  toast(t('dupTrashed', { n: fmtN(res.done), size: fmtBytes(res.freed) }) + (left.size ? ' ' + t('dupTrashSkipped', { n: left.size }) : ''), !!res.failed.length);
}

function dupGroupNode(g, i) {
  const d = dupState();
  const keep = d.keep[i];
  return h('section', { class: 'dup-group' },
    h('div', { class: 'dup-head row between' },
      h('span', {}, h('strong', {}, t('dupGroupTitle', { n: g.files.length })), ' ',
        h('span', { class: 'muted small' }, t('dupGroupSize', { size: fmtBytes(g.size), wasted: fmtBytes(g.wasted) })))),
    g.files.map((f) => {
      const kept = f.path === keep;
      return h('div', { class: 'dup-file' + (kept ? ' kept' : '') },
        h('label', { class: 'dup-keep', title: t('dupKeepHelp') },
          h('input', { type: 'radio', name: 'keep-' + i, checked: kept, 'aria-label': t('dupKeep') + ': ' + f.path,
            onchange: () => { d.keep[i] = f.path; d.drop.delete(f.path); g.files.forEach((x) => { if (x.path !== f.path) d.drop.add(x.path); }); render(); } }),
          h('span', { class: 'small' }, t('dupKeep'))),
        h('label', { class: 'dup-drop', title: t('dupDropHelp') },
          h('input', { type: 'checkbox', checked: !kept && d.drop.has(f.path), disabled: kept, 'aria-label': t('dupDrop') + ': ' + f.path,
            onchange: (e) => { if (e.target.checked) d.drop.add(f.path); else d.drop.delete(f.path); fillDupBar(); } }),
          h('span', { class: 'small' }, t('dupDrop'))),
        h('div', { class: 'pmain' },
          h('span', { class: 'row pname' }, h('span', { class: 'ell mono', title: f.path }, fname(f.path)),
            kept ? h('span', { class: 'chip ok' }, t('dupKept')) : null),
          h('button', { type: 'button', class: 'path-link small ell mono', title: t('revealHere', { path: f.path }), onclick: () => reveal(f.path) },
            icon('folder'), h('span', { class: 'ell' }, f.folder || f.path))),
        h('span', { class: 'muted small nowrap', title: t('dupCreated') }, fmtTime(f.created)));
    }));
}

function fillDupBar() {
  const el = document.getElementById('dup-bar');
  if (!el) return;
  const { n, bytes } = dupSelection();
  fill(el, h('span', {}, n ? t('dupSelected', { n: fmtN(n), size: fmtBytes(bytes) }) : t('dupNothingSelected')),
    h('span', { class: 'row' },
      h('button', { type: 'button', class: 'btn small', onclick: () => { dupDefaults(dupState().data); render(); } }, t('dupSelectCopies')),
      h('button', { type: 'button', class: 'btn small', onclick: () => { dupState().drop.clear(); render(); } }, t('dupSelectNone')),
      h('button', { type: 'button', class: 'btn small primary', disabled: !n, onclick: trashDupSelection }, icon('trash'), t('dupTrash'))));
}

function renderDupsTab() {
  const d = dupState();
  const addFolder = async () => {
    let path = null;
    try { path = await TAURI.dialog.open({ directory: true, multiple: false, title: t('pickFolder') }); }
    catch (e) { toast(String(e), true); return; }
    if (!path || d.folders.some((f) => f.path === path)) return;
    d.folders.push({ path, recursive: true }); saveDupSearch(); render();
  };
  const where = h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('dupWhere')),
      h('span', { class: 'row' },
        h('button', { type: 'button', class: 'btn small', title: t('dupFromRulesHelp'), onclick: () => { d.folders = dupDefaultFolders(); saveDupSearch(); render(); } }, t('dupFromRules')),
        h('button', { type: 'button', class: 'btn small', onclick: addFolder }, icon('folder'), t('addFolder')))),
    d.folders.length ? null : h('p', { class: 'muted pad' }, t('dupNoFolders')),
    d.folders.map((f, i) => h('div', { class: 'folder-row' },
      textInput(f.path, (v) => { f.path = v; saveDupSearch(); }, t('folderPath'), { class: 'inp mono' }),
      check(t('withSubfolders'), f.recursive !== false, (v) => { f.recursive = v; saveDupSearch(); }),
      iconBtn('x', t('removeFolder'), () => { d.folders.splice(i, 1); saveDupSearch(); render(); }))),
    h('div', { class: 'row wrap pad between' },
      h('label', { class: 'row small' }, t('dupMin'),
        select(DUP_MINS.map(([v, k]) => [String(v), t(k)]), String(d.min), (v) => { d.min = parseInt(v, 10) || 0; saveDupSearch(); }, t('dupMin'))),
      h('span', { class: 'row' },
        info(t('tabDups'), t('dupHelpPage')),
        h('button', { type: 'button', class: 'btn primary', disabled: d.loading, onclick: runDupSearch }, t(d.data ? 'dupSearchAgain' : 'dupSearch')))),
    h('div', { id: 'dup-progress', class: 'pv-progress-row' }));

  const data = d.data;
  let results = null;
  if (d.error) results = h('section', { class: 'card' }, h('p', { class: 'warn pad' }, t('previewError', { msg: d.error })));
  else if (data) {
    const shown = data.groups.slice(0, d.shown);
    results = [
      h('section', { class: 'card' + (d.loading ? ' stale' : '') },
        h('div', { class: 'ch' }, h('span', {}, t('dupResults')),
          h('span', { class: 'muted small' }, data.total_groups
            ? t('dupSummary', { g: fmtN(data.total_groups), c: fmtN(data.copies), size: fmtBytes(data.wasted) })
            : t('dupNone'))),
        data.stopped ? h('p', { class: 'warn-row pad' }, t('dupStopped')) : null,
        data.unavailable.length ? h('p', { class: 'warn pad' }, t('homeUnavailable', { list: data.unavailable.join(', ') })) : null,
        data.groups.length < data.total_groups ? h('p', { class: 'muted pad' }, t('dupTopOnly', { n: fmtN(data.groups.length) })) : null,
        data.groups.length ? h('div', { id: 'dup-bar', class: 'row wrap between dup-bar' }) : null,
        h('div', { class: 'dup-list' }, shown.map((g, i) => dupGroupNode(g, i))),
        data.groups.length > d.shown ? h('div', { class: 'row pad' },
          h('button', { type: 'button', class: 'btn small', onclick: () => { d.shown += DUP_PAGE; render(); } },
            t('dupMore', { n: fmtN(Math.min(DUP_PAGE, data.groups.length - d.shown)), left: fmtN(data.groups.length - d.shown) }))) : null),
    ];
  }
  return h('div', { class: 'page dups-page' }, where, results);
}

// ---------- Search tab ----------
const SEARCH_SIZES = [['any', 'sizeAny', null, null], ['small', 'sizeSmall', null, 1 << 20], ['mid', 'sizeMid', 1 << 20, 100 * (1 << 20)],
  ['big', 'sizeBig', 100 * (1 << 20), null], ['huge', 'sizeHuge', 1 << 30, null]];
const SEARCH_PAGE = 500;
const SEARCH_DATES = [['any', 'dateAny'], ['today', 'dateToday'], ['7', 'date7'], ['30', 'date30'], ['365', 'dateYear'], ['old', 'dateOld']];

function searchState() {
  if (!S.search) S.search = { text: '', ext: '', size: 'any', date: 'any', sort: { key: 'name', desc: false }, limit: SEARCH_PAGE, data: null, error: null, loading: false, seq: 0, stamp: null };
  return S.search;
}

/// The folders of all rules (hidden ones only while unlocked: the window never gets them otherwise).
function searchFolders() {
  const out = [];
  for (const r of S.doc.rules) for (const f of r.folders) {
    if (f.path && !out.some((x) => x.path.toLowerCase() === f.path.toLowerCase() && x.recursive === (f.recursive !== false))) out.push({ path: f.path, recursive: f.recursive !== false });
  }
  return out;
}

function searchRequest(refresh) {
  const q = searchState();
  const [, , min, max] = SEARCH_SIZES.find(([k]) => k === q.size) || SEARCH_SIZES[0];
  const now = Math.floor(Date.now() / 1000);
  const day0 = new Date(); day0.setHours(0, 0, 0, 0);
  let after = null, before = null;
  if (q.date === 'today') after = Math.floor(day0 / 1000);
  else if (q.date === 'old') before = now - 365 * 86400;
  else if (q.date !== 'any') after = now - parseInt(q.date, 10) * 86400;
  return { folders: searchFolders(), text: q.text, extensions: q.ext, min_size: min, max_size: max, after, before,
    sort: q.sort.key, desc: q.sort.desc, refresh: !!refresh, limit: q.limit };
}

let searchTimer = null;
function scheduleSearch() { clearTimeout(searchTimer); searchTimer = setTimeout(() => runSearch(), 200); }

/// What the daemon has done so far: when it changes while the page is open, the results
/// are looked up again (the index follows the journal, so this is quick).
const daemonStamp = () => { const d = (S.status && S.status.daemon) || {}; return `${d.day}|${d.done || 0}|${d.failed || 0}`; };

/// A new query starts again with the first page of results.
function newQuery() { searchState().limit = SEARCH_PAGE; }

/// Runs the search; while the folders are being listed, shows how far it got. A newer
/// search supersedes an older one.
async function runSearch(refresh = false) {
  const q = searchState();
  const req = searchRequest(refresh);
  if (!req.folders.length) { q.data = null; q.error = 'no_folders'; fillSearchResults(); return; }
  const my = ++q.seq;
  q.stamp = daemonStamp();
  q.loading = true; fillSearchResults();
  const poll = setInterval(async () => {
    try {
      const p = JSON.parse(await invoke('search_progress'));
      const el = document.getElementById('search-progress');
      if (el && p.phase === 'list') el.textContent = t('searchListing', { n: fmtN(p.total), d: fmtN(p.checked) });
    } catch (_) { /* the result comes anyway */ }
  }, 300);
  try {
    const data = JSON.parse(await invoke('search_files', { text: JSON.stringify(req) }));
    if (my !== q.seq) return;
    q.data = data; q.error = null;
  } catch (e) {
    if (my !== q.seq) return;
    q.error = String(e);
  } finally {
    clearInterval(poll);
    if (my === q.seq) { q.loading = false; fillSearchResults(); }
  }
}

async function openFile(p) {
  try { await invoke('open_file', { path: p }); } catch (e) {
    if (String(e) !== 'gone') { toast(String(e), true); return; }
    toast(t('searchGone'), true);
    const q = searchState();
    if (q.data) { q.data.items = q.data.items.filter((f) => f.path !== p); q.data.total = Math.max(0, q.data.total - 1); fillSearchResults(); }
  }
}

function searchMenu(ev, f) {
  contextMenu(ev, [
    { label: t('ctxOpen'), run: () => openFile(f.path) },
    { label: t('ctxReveal'), run: () => reveal(f.path) },
    { label: t('ctxCopyPath'), run: () => copyPath(f.path) },
  ]);
}

/// Leaving the page keeps the index for a while (going back is instant), then frees it.
let searchForget = null;
function leftSearch() {
  clearTimeout(searchForget);
  searchForget = setTimeout(() => { if (S.tab !== 'search') { invoke('forget_search').catch(() => {}); if (S.search) S.search.data = null; } }, 10 * 60 * 1000);
}

function fillSearchResults() {
  const box = document.getElementById('search-results');
  if (!box) return;
  const q = searchState();
  const data = q.data;
  box.classList.toggle('stale', q.loading && !!data);
  if (q.error) {
    fill(box, h('p', { class: q.error === 'no_folders' ? 'muted pad' : 'warn pad' },
      q.error === 'no_folders' ? t('searchNoFolders') : q.error === 'stopped' ? t('searchStopped') : t('previewError', { msg: q.error })));
    return;
  }
  if (!data) { fill(box, h('p', { id: 'search-progress', class: 'muted pad' }, t('searchStarting'))); return; }
  const rows = data.items.map((f) => h('div', { class: 'frow srow', title: t('searchRowHint'), oncontextmenu: (ev) => searchMenu(ev, f), ondblclick: () => openFile(f.path) },
    h('span', { class: 'ell', title: f.name }, f.name),
    h('span', { class: 'ell' }, placeLink(f.path, f.folder, 'small mono')),
    h('span', { class: 'muted' }, fmtBytes(f.size)),
    h('span', { class: 'muted' }, fmtTime(f.modified))));
  fill(box,
    h('div', { class: 'row between pad-h search-meta' },
      h('span', { id: 'search-progress', class: 'muted small' },
        q.loading ? t('searchSearching') : data.total === data.indexed ? t('searchAll', { n: fmtN(data.total) })
          : t('searchCount', { n: fmtN(data.total), total: fmtN(data.indexed) })),
      h('span', { class: 'muted small' }, t('searchIndexed', { when: relTime(data.built_at) }))),
    data.unavailable.length ? h('p', { class: 'warn pad' }, t('homeUnavailable', { list: data.unavailable.join(', ') })) : null,
    data.capped ? h('p', { class: 'warn pad' }, t('searchCapped', { n: fmtN(data.indexed) })) : null,
    h('div', { class: 'frow fhead muted srow' },
      [['name', 'colName'], ['folder', 'colFolder'], ['size', 'colSize'], ['modified', 'colModified']]
        .map(([k, label]) => sortHeader(label, k, q.sort, (v) => { q.sort = v; newQuery(); runSearch(); }))),
    rows.length ? h('div', { class: 'flist search-list' }, rows,
      data.total > data.items.length ? h('div', { class: 'row pad search-more' },
        h('span', { class: 'muted small' }, t('searchTop', { n: fmtN(data.items.length), total: fmtN(data.total) })),
        h('button', { type: 'button', class: 'btn small', disabled: q.loading || q.limit >= 20000,
          onclick: () => { q.limit = Math.min(20000, q.limit + SEARCH_PAGE * 2); runSearch(); } }, t('searchMore'))) : null)
      : h('p', { class: 'muted pad' }, t('searchNone')));
}

function renderSearchTab() {
  const q = searchState();
  const folders = searchFolders();
  const set = (k) => (v) => { q[k] = v; newQuery(); scheduleSearch(); };
  const head = h('section', { class: 'card' },
    h('div', { class: 'cbody' },
      h('div', { class: 'row search-row' },
        h('input', { id: 'search-q', class: 'inp grow search-q', type: 'search', value: q.text, placeholder: t('searchPh'), 'aria-label': t('tabSearch'),
          spellcheck: false, autofocus: true, oninput: (e) => set('text')(e.target.value) }),
        info(t('tabSearch'), t('searchHelp'))),
      h('div', { class: 'row wrap search-filters' },
        h('label', { class: 'row small' }, t('searchExt'),
          textInput(q.ext, set('ext'), t('searchExt'), { placeholder: 'pdf, docx', class: 'inp ext-inp' })),
        h('label', { class: 'row small' }, t('searchSize'),
          select(SEARCH_SIZES.map(([k, l]) => [k, t(l)]), q.size, (v) => { q.size = v; newQuery(); runSearch(); }, t('searchSize'))),
        h('label', { class: 'row small' }, t('searchDate'),
          select(SEARCH_DATES.map(([k, l]) => [k, t(l)]), q.date, (v) => { q.date = v; newQuery(); runSearch(); }, t('searchDate'))),
        h('span', { class: 'grow' }),
        h('span', { class: 'muted small', title: folders.map((f) => f.path).join('\n') }, t('searchWhere', { n: folders.length })),
        h('button', { type: 'button', class: 'btn small', title: t('searchRefreshHelp'), onclick: () => runSearch(true) }, t('searchRefresh')))));
  return h('div', { class: 'page search-page' }, head, h('section', { id: 'search-results', class: 'card' }));
}

// ---------- fila Variabile ----------
function renderVarsTab() {
  const names = Object.keys(S.doc.dictionaries);
  if (!names.includes(S.dictSel)) S.dictSel = names[0] ?? null;
  const d = S.dictSel ? S.doc.dictionaries[S.dictSel] : null;
  const list = h('aside', { class: 'rules-list' },
    h('div', { class: 'row between pad-h' }, h('strong', {}, t('dictionaries')), h('span', { class: 'muted' }, names.length)),
    names.length === 0 ? h('p', { class: 'muted pad' }, t('noDicts')) : null,
    names.map((n) => h('button', { type: 'button', class: 'rule-item' + (n === S.dictSel ? ' sel' : ''), title: t('ctxHint'),
      onclick: () => { S.dictSel = n; render(); }, oncontextmenu: (e) => dictMenu(e, n) },
      h('span', { class: 'rname ell' }, n),
      h('span', { class: 'muted small' }, t(VAR_KIND_LABEL[varKind(S.doc.dictionaries[n])])))),
    h('button', { type: 'button', class: 'btn add', onclick: () => newDict() }, icon('plus'), t('newDict')),
    h('div', { class: 'row list-tools' },
      h('button', { type: 'button', class: 'btn small grow', onclick: importDicts }, t('importDicts')),
      h('button', { type: 'button', class: 'btn small grow', disabled: names.length === 0, onclick: () => exportDicts(names) }, t('exportAllDicts'))));
  if (!d) {
    return h('div', { class: 'rules-layout' }, list, h('main', { class: 'editor empty' },
      h('div', { class: 'var-empty' },
        h('p', {}, t('noDictsHelp')),
        h('button', { type: 'button', class: 'btn primary', onclick: () => newDict() }, icon('plus'), t('newDict')))));
  }

  const users = dictUsers(S.dictSel);
  const name = S.dictSel;
  const tok = `{var:${name}}`;
  const back = S.varBack && S.doc.rules[S.varBack.sel];
  const steps = h('section', { class: 'card' },
    h('div', { class: 'cbody var-steps' },
      varStep(1, t('varStepWhat'), renderVarWhat(d)),
      varStep(2, t('varStepWhere'),
        select(TEXT_FIELDS.map((k) => [k, t(FIELD_LABEL[k])]), d.field, (v) => { d.field = v; markDirty(); render(); }, t('dictField')),
        h('p', { class: 'hint' }, t(d.field === 'content' ? 'varWhereContent' : 'varWhereName'))),
      d.extract ? varStep(3, t('varStepWhich'), renderVarWhich(d)) : null,
      varStep(d.extract ? 4 : 3, t('varStepNone'),
        textInput(d.default ?? '', (v) => { d.default = v === '' ? null : v; markDirty(); schedulePreview(); }, t('dictDefault'), { placeholder: t('dictDefaultNone') }),
        h('p', { class: 'hint' }, t('varNoneHelp')))));
  const use = h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, t('varUseTitle'))),
    h('div', { class: 'cbody' },
      h('div', { class: 'row wrap var-use' },
        h('code', { class: 'var-tok-big' }, tok),
        h('button', { type: 'button', class: 'btn small', onclick: () => copyPath(tok) }, t('copy'))),
      h('p', { class: 'hint' }, t('varUseHelp')),
      h('p', { class: users.length ? 'small' : 'muted small' },
        users.length ? t('dictUsedBy', { rules: users.map((r) => r.name).join(', ') }) : t('dictUnused'))));
  const json = h('details', { class: 'card var-json', open: !!S.varJsonOpen, ontoggle: (e) => { S.varJsonOpen = e.target.open; } },
    h('summary', { class: 'ch' }, t('dictJsonTitle')), renderDictJsonCard(name, d, true));
  const main = h('main', { class: 'editor' },
    back ? h('div', { class: 'var-back' },
      h('button', { type: 'button', class: 'btn small', onclick: () => { const sel = S.varBack.sel; S.varBack = null; S.sel = sel; setTab('rules'); } },
        '← ', t('varBack', { rule: back.name })),
      h('span', { class: 'muted small' }, t('varBackHint'))) : null,
    h('div', { class: 'row name-row' },
      h('input', { class: 'inp name dict-name', type: 'text', value: name, 'aria-label': t('dictName'),
        onchange: (e) => { if (!renameDict(name, e.target.value)) e.target.value = name; },
        onkeydown: (e) => { if (e.key === 'Enter') e.target.blur(); } }),
      h('button', { type: 'button', class: 'btn', onclick: () => duplicateDict(name) }, t('duplicate')),
      h('button', { type: 'button', class: 'btn danger', onclick: () => deleteDict(name) }, t('deleteDict'))),
    h('p', { class: 'hint' }, t('varIntro')),
    h('div', { class: 'var-layout' },
      h('div', { class: 'var-col' }, steps, use),
      h('div', { class: 'var-col var-side' }, renderVarPreview(name, d))),
    json);
  return h('div', { class: 'rules-layout' }, list, main);
}

function varStep(n, title, ...body) {
  return h('div', { class: 'var-step' },
    h('div', { class: 'var-step-n', 'aria-hidden': 'true' }, String(n)),
    h('div', { class: 'var-step-body' }, h('div', { class: 'var-step-title' }, title), ...body));
}

// ---------- variables: kinds, extraction, preview ----------
const VAR_KINDS = ['date', 'words', 'pattern', 'regex'];
const VAR_KINDS_UI = ['dict', ...VAR_KINDS];
const VAR_KIND_LABEL = { dict: 'varKindDict', date: 'varKindDate', words: 'varKindWords', pattern: 'varKindPattern', regex: 'varKindRegex' };
const VAR_KIND_HELP = { dict: 'varKindDictHelp', date: 'varKindDateHelp', words: 'varKindWordsHelp', pattern: 'varKindPatternHelp', regex: 'varKindRegexHelp' };
const DATE_FORMATS = ['AAAA-LL-ZZ', 'ZZ.LL.AAAA', 'AAAA-LL', 'AAAA', 'LLLL AAAA'];

const varKind = (d) => (d && d.extract ? d.extract.kind : 'dict');

function setVarKind(d, kind) {
  if (kind === varKind(d)) return;
  const fromDict = !d.extract;
  if (kind === 'dict') {
    delete d.extract;
    if (!d.entries.length) d.entries.push(['', '']);
    return;
  }
  const old = d.extract || {};
  d.extract = { kind, occurrence: old.occurrence ?? 1, before: old.before ?? '', after: old.after ?? '' };
  if (kind === 'date') d.extract.format = fmtTok('AAAA-LL-ZZ');
  if (kind === 'words') d.extract.words = [];
  if (kind === 'pattern') d.extract.pattern = '';
  if (kind === 'regex') d.extract.regex = '';
  // Extracted values usually come from the document; a dictionary's entries are not used.
  if (fromDict && d.field === 'extension') d.field = 'content';
  d.entries = d.entries.filter((e) => e[0] || e[1]);
}

/// Step 1: the kind and what it looks for.
function renderVarWhat(d) {
  const kind = varKind(d);
  const kinds = h('div', { class: 'seg var-kinds', role: 'group', 'aria-label': t('varKind') },
    VAR_KINDS_UI.map((k) => h('button', { type: 'button', class: k === kind ? 'on' : '', 'aria-pressed': String(k === kind),
      onclick: () => { setVarKind(d, k); markDirty(); render(); } }, t(VAR_KIND_LABEL[k]))));
  return h('div', {}, kinds, h('p', { class: 'hint' }, t(VAR_KIND_HELP[kind])), d.extract ? renderExtractWhat(d) : renderDictEntries(d));
}

function renderDictEntries(d) {
  return h('div', {},
    h('div', { class: 'dict-head muted' }, h('span', {}, t('dictKey')), h('span', {}, t('dictValue')), h('span')),
    d.entries.map((en, i) => h('div', { class: 'dict-row' },
      textInput(en[0], (v) => { en[0] = v; markDirty(); schedulePreview(); }, t('dictKey'), { placeholder: t('dictKeyPh') }),
      textInput(en[1], (v) => { en[1] = v; markDirty(); schedulePreview(); }, t('dictValue')),
      iconBtn('x', t('removeEntry'), () => { d.entries.splice(i, 1); markDirty(); render(); }))),
    h('p', { class: 'hint' }, t('dictCommaHint')),
    h('div', { class: 'row' }, h('button', { type: 'button', class: 'btn', onclick: () => { d.entries.push(['', '']); markDirty(); render(); } }, t('addEntry'))));
}

/// The signs of a pattern, with what they stand for.
const PATTERN_SIGNS = [['#', 'patDigit'], ['@', 'patLetter'], ['?', 'patAny'], ['%', 'patNumber'], ['*', 'patText'], [' ', 'patSpace']];

/// A pattern in words: "INV-####" → „INV-” + 4 cifre.
function describePattern(p) {
  const parts = [];
  let lit = '';
  const [q1, q2] = LANG === 'en' ? ['“', '”'] : ['„', '”'];
  const flush = () => { if (lit) { parts.push(q1 + lit + q2); lit = ''; } };
  for (let i = 0; i < p.length;) {
    const c = p[i];
    if (c === '\\' && i + 1 < p.length) { lit += p[i + 1]; i += 2; continue; }
    let j = i;
    while (j < p.length && p[j] === c) j++;
    const n = j - i;
    if (c === '#') { flush(); parts.push(t(n === 1 ? 'patDigit1' : 'patDigitN', { n })); }
    else if (c === '@') { flush(); parts.push(t(n === 1 ? 'patLetter1' : 'patLetterN', { n })); }
    else if (c === '?') { flush(); parts.push(t(n === 1 ? 'patAny1' : 'patAnyN', { n })); }
    else if (c === '%') { flush(); parts.push(t('patNumber1')); j = i + 1; }
    else if (c === '*') { flush(); parts.push(t('patText1')); }
    else if (/\s/.test(c)) { flush(); parts.push(t('patSpace1')); }
    else { lit += p.slice(i, j); }
    i = j;
  }
  flush();
  return parts.join(' + ');
}

function insertAtCaret(input, text) {
  const st = input.selectionStart ?? input.value.length;
  const en = input.selectionEnd ?? st;
  input.value = input.value.slice(0, st) + text + input.value.slice(en);
  input.focus();
  input.setSelectionRange(st + text.length, st + text.length);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

function renderExtractWhat(d) {
  const x = d.extract;
  const upd = (k) => (v) => { x[k] = v; markDirty(); schedulePreview(); };
  if (x.kind === 'date') {
    const fmts = DATE_FORMATS.map(fmtTok);
    return h('div', {},
      h('label', { class: 'lbl' }, t('varDateFormat'),
        h('div', { class: 'row' },
          textInput(x.format ?? '', upd('format'), t('varDateFormat'), { placeholder: fmtTok('AAAA-LL-ZZ'), class: 'inp mono' }),
          select([['', t('varFormatPresets')], ...fmts.map((f) => [f, `${f}  →  ${fmtDate(Math.floor(Date.now() / 1000), f)}`])], '',
            (v) => { if (v) { x.format = v; markDirty(); render(); schedulePreview(); } }, t('varFormatPresets')))),
      h('p', { class: 'hint' }, t('varDateHelp')));
  }
  if (x.kind === 'words') {
    const ta = h('textarea', { class: 'inp', rows: 5, 'aria-label': t('varWords'), spellcheck: false, placeholder: 'Enel\nDigi\nOrange',
      oninput: (e) => { x.words = e.target.value.split('\n').map((w) => w.trim()).filter(Boolean); markDirty(); schedulePreview(); } });
    ta.value = (x.words || []).join('\n');
    return h('div', {}, h('label', { class: 'lbl' }, t('varWords'), ta), h('p', { class: 'hint' }, t('varWordsHelp')));
  }
  if (x.kind === 'pattern') {
    const said = h('p', { class: 'small var-said' });
    const tellIt = () => { const v = x.pattern || ''; said.textContent = v ? t('patSays', { what: describePattern(v) }) : ''; };
    const input = textInput(x.pattern ?? '', (v) => { upd('pattern')(v); tellIt(); }, t('varPattern'), { placeholder: 'INV-####', class: 'inp mono' });
    tellIt();
    return h('div', {},
      h('label', { class: 'lbl' }, t('varPattern'), input),
      h('div', { class: 'row wrap var-signs' },
        PATTERN_SIGNS.map(([sign, key]) => h('button', { type: 'button', class: 'btn small sign', title: t(key), onclick: () => insertAtCaret(input, sign) },
          h('span', { class: 'mono' }, sign === ' ' ? '␣' : sign), ' ', t(key)))),
      said,
      h('p', { class: 'hint' }, t('varPatternHelp')));
  }
  return h('div', {},
    h('label', { class: 'lbl' }, t('varRegex'), textInput(x.regex ?? '', upd('regex'), t('varRegex'), { placeholder: 'Nr\\.?\\s*(\\d+)', class: 'inp mono' })),
    h('p', { class: 'hint' }, t('varRegexHelp')));
}

/// Step 3: which match counts.
function renderVarWhich(d) {
  const x = d.extract;
  const upd = (k) => (v) => { x[k] = v; markDirty(); schedulePreview(); };
  const occ = x.occurrence === 'last' ? 'last' : String(x.occurrence || 1);
  const occOpts = [...Array.from({ length: 10 }, (_, i) => [String(i + 1), i === 0 ? t('occFirst') : t('occNth', { n: i + 1 })]), ['last', t('occLast')]];
  if (!occOpts.some(([v]) => v === occ)) occOpts.splice(10, 0, [occ, t('occNth', { n: occ })]);
  return h('div', {},
    h('div', { class: 'grid3' },
      h('label', { class: 'lbl' }, t('varOccurrence'),
        select(occOpts, occ, (v) => { x.occurrence = v === 'last' ? 'last' : parseInt(v, 10); markDirty(); schedulePreview(); }, t('varOccurrence'))),
      h('label', { class: 'lbl' }, t('varBefore'), textInput(x.before ?? '', upd('before'), t('varBefore'), { placeholder: t('varBeforePh') })),
      h('label', { class: 'lbl' }, t('varAfter'), textInput(x.after ?? '', upd('after'), t('varAfter'), { placeholder: t('varAfterPh') }))),
    h('p', { class: 'hint' }, t('varLabelHelp')));
}

let previewTimer = null;
function schedulePreview() {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(runVarPreview, 250);
}

/// Shows what the selected variable finds in the preview text.
async function runVarPreview() {
  const out = document.getElementById('var-pv-out');
  const name = S.dictSel;
  const d = name ? S.doc.dictionaries[name] : null;
  if (!out || !d) return;
  const text = S.varPv.text || '';
  if (!text) { out.replaceChildren(h('p', { class: 'muted' }, t('varPreviewEmpty'))); return; }
  let res;
  try { res = JSON.parse(await invoke('test_variable', { def: JSON.stringify(d), text, lang: LANG })); }
  catch (e) { out.replaceChildren(h('p', { class: 'err' }, String(e))); return; }
  if (S.dictSel !== name) return;
  if (res.error) { out.replaceChildren(h('p', { class: 'err' }, res.error)); return; }
  const head = res.found
    ? h('div', { class: 'var-result ok' }, h('span', { class: 'muted small' }, t('varResult')), h('strong', { class: 'mono' }, res.value))
    : h('div', { class: 'var-result none' }, d.default ? t('varResultDefault', { d: d.default }) : t('varResultNone'));
  // The text with every match marked; the one used is emphasised.
  const parts = [];
  let at = 0;
  const shown = text.slice(0, 20000);
  (res.matches || []).filter((m) => m.end <= shown.length).forEach((m, i) => {
    if (m.start > at) parts.push(shown.slice(at, m.start));
    parts.push(h('mark', { class: i === res.used ? 'used' : '', title: m.value }, shown.slice(m.start, m.end)));
    at = m.end;
  });
  parts.push(shown.slice(at));
  out.replaceChildren(...[head,
    res.matches && res.matches.length > 1 ? h('p', { class: 'muted small' }, t('varMatches', { n: res.matches.length })) : null,
    d.extract ? h('pre', { class: 'var-pv-text' }, parts) : null,
    text.length > shown.length ? h('p', { class: 'muted small' }, t('varPreviewCut')) : null].filter(Boolean));
}

function renderVarPreview(name, d) {
  if (S.varPv.name !== name) S.varPv = { name, text: S.varPv.text || '' };
  const byName = d.field !== 'content';
  const ta = h('textarea', { id: 'var-pv-ta', class: 'inp', rows: byName ? 2 : 7, spellcheck: false, 'aria-label': t('varPreview'),
    placeholder: t(byName ? 'varPreviewPhName' : 'varPreviewPh'),
    oninput: (e) => { S.varPv.text = e.target.value; schedulePreview(); } });
  ta.value = S.varPv.text;
  const load = async () => {
    try {
      const path = await TAURI.dialog.open({ multiple: false, title: t('varLoadFile') });
      if (!path) return;
      if (byName) {
        const file = String(path).split(/[\\/]/).pop();
        S.varPv.text = d.field === 'name' ? stemOf(file) : d.field === 'extension' ? extOf(file)
          : d.field === 'folder' ? String(path).slice(0, String(path).length - file.length - 1) : String(path);
      } else {
        S.varPv.text = await invoke('file_text', { path });
      }
      ta.value = S.varPv.text;
      runVarPreview();
    } catch (e) { toast(String(e) === 'no_text' ? t('varNoText') : String(e), true); }
  };
  setTimeout(runVarPreview, 0);
  return h('section', { class: 'card var-pv' },
    h('div', { class: 'ch' }, h('span', {}, t('varPreview')),
      h('button', { type: 'button', class: 'btn small', onclick: load }, t(byName ? 'varLoadName' : 'varLoadFile'))),
    h('div', { class: 'cbody' }, h('p', { class: 'hint' }, t(byName ? 'varPreviewHelpName' : 'varPreviewHelp')), ta,
      h('div', { id: 'var-pv-out', class: 'var-pv-out' })));
}

// ---------- variables: ready-made starting points ----------
const VAR_SAMPLE = {
  ro: 'FACTURĂ FISCALĂ\nFurnizor: Enel Energie Muntenia SA\nSeria ENL nr. 0045213\nData emiterii: 01.11.2026\nData scadentă: 30.11.2026\nTotal de plată: 245,30 lei',
  en: 'INVOICE\nSupplier: Enel Energy Ltd\nInvoice no. INV-2026-0042\nIssue date: 1 November 2026\nDue date: November 30, 2026\nTotal due: 245.30 EUR',
};

function varTemplates() {
  const en = LANG === 'en';
  const sample = VAR_SAMPLE[en ? 'en' : 'ro'];
  const x = (kind, more) => ({ kind, occurrence: 1, before: '', after: '', ...more });
  const fromText = (extract, dflt = null) => ({ field: 'content', entries: [], default: dflt, extract });
  return [
    { id: 'due', name: en ? 'DueDate' : 'DataScadenta', sample,
      def: fromText(x('date', { format: fmtTok('AAAA-LL-ZZ'), before: en ? 'Due date:' : 'Data scadentă:' })) },
    { id: 'date', name: en ? 'DocDate' : 'DataDoc', sample, def: fromText(x('date', { format: fmtTok('AAAA-LL') })) },
    { id: 'number', name: en ? 'InvoiceNo' : 'NrFactura', sample,
      def: fromText(x('regex', { regex: '\\b(?:nr|no)\\.?\\s*:?\\s*([A-Z]{0,5}[ -]?\\d{3,}[0-9/-]*)' })) },
    { id: 'supplier', name: en ? 'Supplier' : 'Furnizor', sample,
      def: fromText(x('words', { words: ['Enel', 'E.ON', 'Engie', 'Digi', 'Orange', 'Vodafone'] }), en ? 'Other' : 'Altii') },
    { id: 'total', name: en ? 'Total' : 'Total', sample,
      def: fromText(x('pattern', { pattern: en ? '% EUR' : '% lei' })) },
    { id: 'year', name: en ? 'Year' : 'An', sample: en ? 'sales_report_2025_final' : 'raport_vanzari_2025_final',
      def: { field: 'name', entries: [], default: null, extract: x('regex', { regex: '(?:^|[^0-9])((?:19|20)\\d\\d)(?:[^0-9]|$)' }) } },
    { id: 'type', name: en ? 'FileType' : 'TipFisier', sample: 'pdf',
      def: { field: 'extension', default: en ? 'Other' : 'Altele', entries: [
        ['pdf, doc, docx, odt, txt', en ? 'Documents' : 'Documente'], ['jpg, jpeg, png, heic, gif', en ? 'Pictures' : 'Poze'],
        ['mp4, mkv, avi, mov', en ? 'Videos' : 'Video'], ['zip, rar, 7z', en ? 'Archives' : 'Arhive']] } },
  ];
}

/// Asks where a new variable starts from: an example or an empty one of a kind.
function chooseVarStart() {
  return new Promise((resolve) => {
    const root = document.getElementById('modal-root');
    const done = (v) => { root.replaceChildren(); window.removeEventListener('keydown', onKey, true); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') { e.preventDefault(); done(null); } };
    window.addEventListener('keydown', onKey, true);
    fill(root, h('div', { class: 'overlay', onclick: (e) => { if (e.target === e.currentTarget) done(null); } },
      h('div', { class: 'modal var-start', role: 'dialog', 'aria-modal': 'true', 'aria-label': t('varStartTitle') },
        h('h3', {}, t('varStartTitle')),
        h('p', { class: 'muted' }, t('varStartHelp')),
        h('div', { class: 'var-tpls' }, varTemplates().map((tp) => h('button', { type: 'button', class: 'var-tpl', 'data-tpl': tp.id, onclick: () => done({ tpl: tp }) },
          h('strong', {}, t('tpl_' + tp.id)), h('span', { class: 'muted small' }, t('tpl_' + tp.id + '_d'))))),
        h('p', { class: 'small var-start-or' }, t('varStartBlank')),
        h('div', { class: 'row wrap' }, VAR_KINDS_UI.map((k) => h('button', { type: 'button', class: 'btn small', 'data-kind': k, onclick: () => done({ kind: k }) }, t(VAR_KIND_LABEL[k])))),
        h('div', { class: 'row end' }, h('button', { type: 'button', class: 'btn', onclick: () => done(null) }, t('cancel'))))));
    const first = root.querySelector('.var-tpl'); if (first) first.focus();
  });
}

/// Adds the chosen variable (not yet shown) and returns its name.
function createVar(choice) {
  let name, def;
  if (choice.tpl) {
    name = uniqueDictName(choice.tpl.name);
    def = clone(choice.tpl.def);
    S.varPv = { name, text: choice.tpl.sample };
  } else {
    name = uniqueDictName(t('varBlankName'));
    def = { field: 'extension', entries: [['', '']], default: null };
    setVarKind(def, choice.kind);
    S.varPv = { name, text: choice.kind === 'dict' ? '' : (S.varPv.text || '') };
  }
  S.doc.dictionaries[name] = def;
  S.dictSel = name;
  markDirty();
  return name;
}

async function newDict() {
  const choice = await chooseVarStart();
  if (!choice) return null;
  const name = createVar(choice);
  render();
  return name;
}

/// From a rule: makes a variable, lets the rule use it (`use(name)`), then opens it in the
/// Variables page with a way back to the rule.
async function newVarFromRule(use) {
  const sel = S.sel;
  const choice = await chooseVarStart();
  if (!choice) return;
  const name = createVar(choice);
  use(name);
  S.varBack = { sel };
  S.tab = 'vars';
  render();
}

// ---------- fila Setări ----------
async function importRules() {
  try {
    const path = await TAURI.dialog.open({ multiple: false, filters: [{ name: 'FilePilot', extensions: ['orgrules', 'json'] }] });
    if (!path) return;
    const incoming = JSON.parse(await invoke('read_rules_file', { path }));
    const taken = new Set(S.doc.rules.map((r) => r.name));
    const [, renamed] = mergeDicts(incoming.dictionaries, incoming.rules);
    for (const r of incoming.rules) {
      let n = r.name, k = 2;
      while (taken.has(n)) n = `${r.name} (${k++})`;
      r.name = n; taken.add(n);
      S.doc.rules.push(r);
    }
    markDirty();
    toast(t('importedN', { n: incoming.rules.length }) + (renamed.length ? ' ' + t('dictImportRenamed', { list: renamed.map(([a, b]) => `${a} → ${b}`).join(', ') }) : ''));
    render();
  } catch (e) { toast(String(e), true); }
}

async function exportRules() {
  try {
    const path = await TAURI.dialog.save({ defaultPath: 'reguli.orgrules', filters: [{ name: 'FilePilot', extensions: ['orgrules'] }] });
    if (!path) return;
    await invoke('export_rules', { path, text: JSON.stringify(S.doc) });
    toast(t('exportedOk'));
  } catch (e) { toast(String(e), true); }
}

// ---------- starea daemonului ----------
const localDay = () => { const d = new Date(); return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`; };

/// Ce arată eticheta de sus: [clasă, text, detalii].
function daemonState() {
  const st = S.status;
  if (!st || !st.daemon_running) return ['off', t('daemonOff'), t('daemonOffHelp')];
  const d = st.daemon || {};
  const today = d.day === localDay();
  const done = today ? d.done || 0 : 0, failed = today ? d.failed || 0 : 0;
  const counts = t('todayCounts', { done, failed });
  if (st.paused_until != null) return ['wait', t('daemonPaused'), pauseText(st.paused_until)];
  if (d.last_error) return ['bad', t('daemonError'), d.last_error];
  if (d.unavailable && d.unavailable.length) return ['wait', t('daemonOn'), t('unavailableFolders', { list: d.unavailable.join(', ') })];
  if (d.scan && d.scan.total) {
    const p = Math.min(100, Math.floor((d.scan.done / d.scan.total) * 100));
    return ['ok', t('daemonScanning', { p }), t('progChecking', { d: fmtN(d.scan.done), t: fmtN(d.scan.total), p })];
  }
  return ['ok', t('daemonOn'), counts];
}

function pauseText(until) {
  if (!until) return t('pausedUntilResume');
  const d = new Date(until * 1000);
  return t('pausedUntil', { time: `${pad2(d.getHours())}:${pad2(d.getMinutes())}` });
}

async function setPause(until) {
  try { await invoke('set_pause', { until }); } catch (e) { toast(String(e), true); }
  setTimeout(refreshStatus, 300);
  setTimeout(refreshStatus, 1500);
}
const pauseFor = (secs) => setPause(Math.floor(Date.now() / 1000) + secs);

function daemonDetails() {
  const st = S.status;
  if (!st || !st.daemon_running) return null;
  const d = st.daemon || {};
  const today = d.day === localDay();
  const paused = st.paused_until != null;
  return [
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('statRules')), h('span', {}, t('statRulesVal', { a: d.active ?? '—', n: d.rules ?? '—' }))),
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('statFolders')), h('span', {}, d.folders ? String(d.folders.length) : '—')),
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('statToday')),
      h('span', {}, t('todayVal', { done: today ? d.done || 0 : 0, failed: today ? d.failed || 0 : 0 }))),
    d.started ? h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('statStarted')), h('span', {}, fmtTime(d.started))) : null,
    d.unavailable && d.unavailable.length ? h('p', { class: 'warn' }, t('unavailableFolders', { list: d.unavailable.join(', ') })) : null,
    d.last_error ? h('p', { class: 'warn bad-msg' }, t('lastError', { msg: d.last_error })) : null,
    h('div', { class: 'row wrap' },
      paused ? h('span', { class: 'chip wait' }, pauseText(st.paused_until)) : null,
      paused ? h('button', { type: 'button', class: 'btn primary', onclick: () => setPause(null) }, t('resume')) : [
        h('button', { type: 'button', class: 'btn', onclick: () => pauseFor(3600) }, t('pause1h')),
        h('button', { type: 'button', class: 'btn', onclick: () => setPause(0) }, t('pauseInf'))]),
    h('p', { class: 'hint' }, t('pauseHelp')),
  ];
}

/// Cât stă un fișier nemodificat înainte de procesare.
const SETTLE_OPTIONS = [[0, 'settleNow'], [5, 'settle5s'], [30, 'settle30s'], [60, 'settle1m'], [300, 'settle5m'], [900, 'settle15m'], [3600, 'settle1h']];

const JOURNAL_KEEP = [[30, 'keep30'], [90, 'keep90'], [365, 'keep365'], [0, 'keepAll']];

async function setJournalDays(days) {
  try {
    await invoke('set_journal_days', { days });
    if (S.status) S.status.journal_days = days;
    render();
    toast(t('journalKeepSaved'));
  } catch (e) { toast(String(e), true); }
}

async function setSettle(secs) {
  try {
    await invoke('set_settle', { secs });
    if (S.status) S.status.settle_secs = secs;
    render();
    toast(t('settleSaved'));
  } catch (e) { toast(String(e), true); }
}

function renderSettingsTab() {
  const st = S.status;
  const card = (title, extra, ...body) => h('section', { class: 'card' },
    h('div', { class: 'ch' }, h('span', {}, title), extra || null), h('div', { class: 'cbody' }, body));
  const [chipCls, chipTxt, chipTip] = daemonState();
  const general = card(t('general'), null,
    h('label', { class: 'lbl' }, t('language'),
      select([['ro', 'Română'], ['en', 'English']], LANG, (v) => { setLang(v); try { localStorage.setItem('lang', v); } catch (_) { /* fără stocare */ } invoke('set_language', { lang: LANG }).catch(() => {}); render(); }, t('language'))),
    h('p', { class: 'hint' }, t('languageHelp')),
    h('label', { class: 'lbl' }, t('theme'),
      select([['auto', t('themeAuto')], ['light', t('themeLight')], ['dark', t('themeDark')]], THEME,
        (v) => { setTheme(v); try { localStorage.setItem('theme', v); } catch (_) { /* fără stocare */ } }, t('theme'))));
  const daemon = card(t('daemon'),
    h('span', { class: 'row' }, h('span', { class: 'chip ' + chipCls, title: chipTip }, chipTxt),
      st && !st.daemon_running ? h('button', { type: 'button', class: 'btn small', onclick: startDaemon }, t('startDaemon')) : null),
    daemonDetails(),
    h('p', { class: 'hint' }, t('daemonHelp')),
    check(t('autostart'), S.autostart, setAutostart),
    h('p', { class: 'hint' }, t('autostartHelp')));
  const cur = st && st.settle_secs != null ? st.settle_secs : 5;
  const keep = st && st.journal_days != null ? st.journal_days : 90;
  const keepOpts = JOURNAL_KEEP.map(([v, k]) => [String(v), t(k)]);
  if (!JOURNAL_KEEP.some(([v]) => v === keep)) keepOpts.push([String(keep), t('keepCustom', { n: keep })]);
  const opts = SETTLE_OPTIONS.map(([v, k]) => [String(v), t(k)]);
  if (!SETTLE_OPTIONS.some(([v]) => v === cur)) opts.push([String(cur), t('settleCustom', { n: cur })]);
  const timing = card(t('processing'), null,
    h('label', { class: 'lbl' }, t('settleLabel'),
      select(opts, String(cur), (v) => setSettle(Number(v)), t('settleLabel'))),
    h('p', { class: 'hint' }, cur === 0 ? t('settleHelpNow') : t('settleHelp')),
    h('label', { class: 'lbl' }, t('journalKeep'),
      select(keepOpts, String(keep), (v) => setJournalDays(Number(v)), t('journalKeep'))),
    h('p', { class: 'hint' }, t('journalKeepHelp')));
  const g = S.gate;
  const hiddenCard = card(t('hiddenRules'),
    g.password ? h('span', { class: 'chip ' + (g.unlocked ? 'wait' : 'ok') }, g.unlocked ? t('unlockedState') : t('lockedState')) : null,
    h('p', {}, g.password ? t('hiddenCount', { n: g.hidden }) : t('noPassword')),
    h('div', { class: 'row wrap' },
      g.password && !g.unlocked ? h('button', { type: 'button', class: 'btn primary', onclick: unlockHidden }, icon('lock'), t('unlock')) : null,
      g.password && g.unlocked ? h('button', { type: 'button', class: 'btn', onclick: lockHidden }, icon('unlock'), t('lock')) : null,
      h('button', { type: 'button', class: 'btn', onclick: setPassword }, g.password ? t('changePassword') : t('setPassword')),
      g.password ? h('button', { type: 'button', class: 'btn danger', onclick: removePassword }, t('removePassword')) : null),
    h('p', { class: 'hint' }, t('hiddenRulesHelp')));
  const io = card(t('importExport'), null,
    h('div', { class: 'row wrap' },
      h('button', { type: 'button', class: 'btn', onclick: importRules }, t('importRules')),
      h('button', { type: 'button', class: 'btn', onclick: exportRules }, t('exportRules'))),
    h('p', { class: 'hint' }, t('importExportHelp')));
  const about = card(t('about'), null,
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('dataFolder')),
      h('span', { class: 'row' }, h('span', { class: 'mono ell', title: st ? st.data_dir : '' }, st ? st.data_dir : '—'),
        st ? h('button', { type: 'button', class: 'btn small', onclick: () => reveal(st.data_dir) }, t('open')) : null)),
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('ruleBackups')),
      h('span', { class: 'row' }, h('span', {}, st ? String(st.backups ?? 0) : '—'),
        st && st.backups ? h('button', { type: 'button', class: 'btn small', onclick: () => reveal(st.backup_dir) }, t('open')) : null)),
    h('p', { class: 'hint' }, t('ruleBackupsHelp')),
    h('div', { class: 'kv' }, h('span', { class: 'muted' }, t('version')), h('span', {}, st ? st.version : '—')));
  return h('div', { class: 'page settings-page' }, h('h2', {}, t('settings')),
    h('div', { class: 'settings-grid' }, h('div', { class: 'col' }, general, io, about), h('div', { class: 'col' }, daemon, timing, hiddenCard)));
}

async function setAutostart(on) {
  try { await invoke('set_autostart', { enabled: on }); S.autostart = on; }
  catch (e) { toast(t('autostartFail', { msg: e }), true); }
  try { S.autostart = await invoke('get_autostart'); } catch (_) { /* rămâne ultima stare */ }
  render();
}

async function startDaemon() {
  try { await invoke('start_daemon'); toast(t('daemonStarted')); } catch (e) { toast(String(e), true); }
  setTimeout(refreshStatus, 800);
}

// ---------- cadru ----------
function refreshSaveState() {
  const b = document.getElementById('save-btn');
  const m = document.getElementById('save-msg');
  if (b) b.disabled = !S.dirty;
  if (m) m.textContent = S.dirty ? t('unsaved') : '';
}

// ---------- regulile ascunse ----------
async function refreshGate() {
  try { S.gate = await jsonCmd('gate'); } catch (_) { /* rămâne cum era */ }
}

/// Documentul de reguli de la început (cu sau fără cele ascunse, după starea porții).
async function reloadDoc() {
  const doc = JSON.parse(await invoke('get_rules'));
  if (!Array.isArray(doc.rules)) doc.rules = [];
  if (!doc.dictionaries || typeof doc.dictionaries !== 'object') doc.dictionaries = {};
  const selId = rule() && rule().id;
  S.doc = doc; S.dirty = false;
  const i = doc.rules.findIndex((r) => r.id && r.id === selId);
  S.sel = i >= 0 ? i : Math.min(S.sel, Math.max(0, doc.rules.length - 1));
  S.pv.data = null;
}

/// Modificările nesalvate se salvează întâi: după deblocare sau blocare documentul se reîncarcă.
async function saveBeforeGate() {
  if (!S.dirty) return true;
  await save();
  return !S.dirty;
}

async function unlockHidden() {
  if (!(await saveBeforeGate())) return;
  const ok = await passwordModal(t('unlockTitle'), [['pw', t('password')]], t('unlock'), async (v) =>
    (await invoke('unlock', { password: v.pw })) ? null : t('wrongPassword'));
  if (!ok) return;
  await refreshGate();
  try { await reloadDoc(); } catch (e) { toast(t('loadError', { msg: e }), true); }
  S.journal = null; render(); runPreview();
  toast(t('unlocked', { n: S.gate.hidden }));
}

async function lockHidden() {
  if (!(await saveBeforeGate())) return;
  await invoke('lock');
  await refreshGate();
  try { await reloadDoc(); } catch (e) { toast(t('loadError', { msg: e }), true); }
  S.journal = null; render(); runPreview();
}

async function setPassword() {
  const has = S.gate.password;
  const fields = [...(has ? [['old', t('currentPassword')]] : []), ['pw', t('newPassword')], ['pw2', t('repeatPassword')]];
  const ok = await passwordModal(has ? t('changePassword') : t('setPassword'), fields, t('ok'), async (v) => {
    if (v.pw.length < 4) return t('passwordShort');
    if (v.pw !== v.pw2) return t('passwordMismatch');
    await invoke('set_hidden_password', { old: has ? v.old : null, new: v.pw });
    return null;
  });
  if (!ok) return;
  await refreshGate(); render(); toast(t('passwordSaved'));
}

async function removePassword() {
  const ok = await passwordModal(t('removePassword'), [['old', t('currentPassword')]], t('removePassword'), async (v) => {
    await invoke('set_hidden_password', { old: v.old, new: null });
    return null;
  });
  if (!ok) return;
  await refreshGate();
  try { await reloadDoc(); } catch (_) { /* rămâne documentul */ }
  render(); toast(t('passwordRemoved'));
}

function gateButton() {
  const g = S.gate;
  if (!g.password) return null;
  return g.unlocked
    ? h('button', { type: 'button', class: 'btn small', title: t('lockHelp'), onclick: lockHidden }, icon('unlock'), t('lock'))
    : h('button', { type: 'button', class: 'btn small', title: t('unlockHelp'), onclick: unlockHidden }, icon('lock'), g.hidden ? String(g.hidden) : null);
}

async function save() {
  if (!S.dirty) return;
  let hiddenNow = false;
  try {
    const saved = await invoke('save_rules', { text: JSON.stringify(S.doc, null, 2) });
    // Documentul scris pe disc are id-urile permanente ale regulilor noi: îl preluăm,
    // altfel o redenumire înainte de următoarea salvare ar schimba id-ul.
    if (typeof saved === 'string' && saved) {
      const doc = JSON.parse(saved);
      if (!doc.dictionaries || typeof doc.dictionaries !== 'object') doc.dictionaries = {};
      // Blocat: o regulă tocmai marcată ascunsă dispare din listă.
      const gone = S.doc.rules.length - doc.rules.length;
      hiddenNow = gone > 0;
      S.doc = doc;
      if (gone > 0) {
        S.sel = Math.min(S.sel, Math.max(0, doc.rules.length - 1));
        await refreshGate();
        toast(t('nowHidden', { n: gone }));
      }
    }
    S.dirty = false;
    render();
    if (!hiddenNow) toast(t('saved'));
  } catch (e) { toast(t('saveFailed') + ': ' + e, true); }
}

/// Ce se vede din stare în fila Setări (fără `updated`/`now`, care se schimbă mereu).
function statusKey(st) {
  if (!st) return 'null';
  const d = st.daemon || {};
  return JSON.stringify([st.daemon_running, st.paused_until, d.rules, d.active, d.folders, d.unavailable, d.day, d.done, d.failed, d.last_error, d.started]);
}

function fillDaemonChip() {
  const chip = document.getElementById('daemon-chip');
  if (!chip) return;
  const [c, txt, tip] = daemonState();
  chip.className = 'chip clickable ' + c;
  chip.textContent = txt;
  chip.title = tip;
}

let fastStatus = null;
async function refreshStatus() {
  const before = statusKey(S.status);
  try { S.status = await jsonCmd('status'); } catch (_) { S.status = null; }
  // Cât daemonul scanează, progresul se urmărește la fiecare secundă.
  const scanning = !!(S.status && S.status.daemon_running && S.status.daemon && S.status.daemon.scan);
  if (scanning && !fastStatus) fastStatus = setTimeout(() => { fastStatus = null; refreshStatus(); }, 1000);
  const n = S.status && S.status.daemon ? S.status.daemon.pending || 0 : null;
  if (n != null && n !== S.pend.total) loadPending();
  fillDaemonChip();
  if (S.tab === 'home') { fillHomeStatus(); fillHomeChecks(); }
  // Doar dacă s-a schimbat ceva: o redesenare închide meniurile derulante deschise.
  if (S.tab === 'settings' && before !== statusKey(S.status) && !document.querySelector('.settings-page select:focus')) render();
}

function setTab(tab) {
  if (tab !== 'vars') S.varBack = null;
  if (S.tab === 'search' && tab !== 'search') leftSearch();
  S.tab = tab;
  render();
  if (tab === 'journal') { loadJournal(); loadPending(); }
  if (tab === 'rules') runPreview();
  if (tab === 'home') { loadHome(); loadPending(); }
  if (tab === 'search') { runSearch(); const el = document.getElementById('search-q'); if (el) el.focus(); }
}

let lastView = null;
const SCROLLERS = ['.editor', '.page', '.rules-list', '.flist'];

function render() {
  ISSUE_SLOTS = [];
  closeInfo();
  closeVarMenu();
  closeCtx();
  // Aceeași vedere (fila și regula selectată): se păstrează derularea, altfel orice
  // adăugare de condiție ar sări la începutul paginii.
  const view = S.tab + ':' + (S.tab === 'vars' ? S.dictSel : S.sel);
  const scroll = view === lastView
    ? SCROLLERS.map((sel) => { const el = document.querySelector(sel); return [sel, el ? el.scrollTop : 0]; })
    : [];
  lastView = view;
  const tabs = [['home', 'tabHome'], ['rules', 'tabRules'], ['journal', 'tabJournal'], ['search', 'tabSearch'], ['dups', 'tabDups'], ['vars', 'tabVars'], ['settings', 'tabSettings']];
  const content = S.tab === 'home' ? renderHomeTab() : S.tab === 'rules' ? renderRulesTab() : S.tab === 'journal' ? renderJournalTab()
    : S.tab === 'search' ? renderSearchTab() : S.tab === 'dups' ? renderDupsTab() : S.tab === 'vars' ? renderVarsTab() : renderSettingsTab();
  fill(document.getElementById('app'),
    h('header', { class: 'topbar', 'data-tauri-drag-region': true },
      h('div', { class: 'brand', 'data-tauri-drag-region': true }, 'FilePilot'),
      h('nav', { class: 'tabs', 'aria-label': 'Secțiuni' }, tabs.map(([k, label]) =>
        h('button', { type: 'button', class: S.tab === k ? 'tab on' : 'tab', onclick: () => setTab(k) }, t(label),
          k === 'home' ? h('span', { id: 'pending-badge', class: 'badge', title: t('pendingBadge'), hidden: true }) : null))),
      h('div', { class: 'grow', 'data-tauri-drag-region': true }),
      h('div', { class: 'row actions' },
        h('span', { id: 'save-msg', class: 'muted' }),
        h('button', { id: 'save-btn', type: 'button', class: 'btn primary', onclick: save }, t('save')),
        h('button', { id: 'daemon-chip', type: 'button', class: 'chip clickable', onclick: () => setTab('settings') })),
      windowControls()),
    content);
  refreshSaveState();
  fillDaemonChip();
  fillPendingBadge();
  if (S.tab === 'rules') fillPreview();
  if (S.tab === 'journal') { renderJournalBody(); fillPending(); }
  if (S.tab === 'home') { fillHomeStatus(); fillHomeWeek(); fillHomeRecent(); fillHomeChecks(); fillPending(); }
  if (S.tab === 'dups') { fillDupProgress(); fillDupBar(); }
  if (S.tab === 'search') fillSearchResults();
  for (const [sel, y] of scroll) { const el = document.querySelector(sel); if (el) el.scrollTop = y; }
  refreshIssues();
}

/// La închiderea ferestrei cu modificări nesalvate se întreabă ce facem.
function guardClose() {
  let w;
  try { w = TAURI.window.getCurrentWindow(); } catch (_) { return; }
  w.onCloseRequested(async (ev) => {
    if (!S.dirty) return;
    ev.preventDefault();
    const choice = await choiceModal(t('unsavedClose'), [
      ['save', t('saveAndClose'), 'primary'], ['discard', t('discardAndClose'), 'danger'], ['cancel', t('cancel'), '']]);
    if (choice === 'save') { await save(); if (!S.dirty) w.destroy(); }
    else if (choice === 'discard') { S.dirty = false; w.destroy(); }
  }).catch(() => {});
}

async function init() {
  let l = 'ro';
  try { l = localStorage.getItem('lang') || (navigator.language || 'ro').slice(0, 2); } catch (_) { /* implicit */ }
  setLang(l);
  invoke('set_language', { lang: LANG }).catch(() => {}); // pentru meniul din tray
  await refreshGate();
  try { await reloadDoc(); } catch (e) { toast(t('loadError', { msg: e }), true); }
  try { S.autostart = await invoke('get_autostart'); } catch (_) { /* implicit: oprit */ }
  await refreshStatus();
  // Daemonul e scopul aplicației: dacă nu rulează, pornește-l (apare pictograma din tray).
  if (S.status && !S.status.daemon_running) {
    invoke('start_daemon').then(() => setTimeout(refreshStatus, 1000)).catch(() => {});
  }
  render();
  runPreview();
  // Fereastra e creată ascunsă (poziția salvată se aplică înainte); o arătăm după primul desen.
  guardClose();
  requestAnimationFrame(() => requestAnimationFrame(() => {
    try { TAURI.window.getCurrentWindow().show().catch(() => {}); } catch (_) { /* fără fereastră Tauri */ }
  }));
  setInterval(refreshStatus, 10000);
  setInterval(() => {
    if (document.hidden) return;
    if (S.tab === 'journal') { loadJournal(); loadPending(); }
    if (S.tab === 'home') { loadHome(); loadPending(); }
    if (S.tab === 'search' && S.search && S.search.data && !S.search.loading && S.search.stamp !== daemonStamp()) runSearch();
  }, 4000);
  loadPending();
  loadHome();
  if (looksNew() && !onboarded()) firstRunWizard();
  window.addEventListener('keydown', (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const k = e.key.toLowerCase();
    if (k === 's') { e.preventDefault(); save(); }
    if (k === 'f') {
      e.preventDefault();
      if (S.tab !== 'search') setTab('search');
      const el = document.getElementById('search-q');
      if (el) { el.focus(); el.select(); }
    }
  });
}

init();
