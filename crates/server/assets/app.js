/* Тайм-лист — веб-интерфейс. Без сборки и зависимостей: один файл, обычный DOM.
 *
 * Все расчёты делает сервер (ядро на Rust). Интерфейс только показывает ответ
 * /api/month/ГГГГ-ММ и отправляет правки; после каждой правки сервер возвращает
 * пересчитанный месяц, поэтому итоги обновляются сразу, как в Excel.
 */
'use strict';

// ------------------------------------------------------------------ утилиты

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

const FIELDS = ['start', 'start_code', 'away', 'away_code', 'end', 'end_code', 'day_code'];
const FIELD_LABEL = { start: 'Начало', start_code: 'код', away: 'Уходы', away_code: 'код', end: 'Конец', end_code: 'код', day_code: 'Код' };
const FIELD_HINT = {
  start: 'Начало рабочего дня', start_code: 'Причина опоздания или отсутствия весь день', away: 'Отлучки в течение дня, ч:мм',
  away_code: 'Причина отлучки', end: 'Конец рабочего дня', end_code: 'Причина раннего ухода', day_code: 'Тариф иногородней командировки',
};
const isTime = (f) => f === 'start' || f === 'away' || f === 'end';
const KIND_LABEL = { arrive: 'Приход', leave: 'Уход', away: 'Выезд', back: 'Возврат', late: 'Опоздание', absent: 'Отсутствие', info: 'Без отметки', unknown: 'Не распознано' };
const AUDIT_LABEL = { ...FIELD_LABEL, start_code: 'код начала', away_code: 'код уходов', end_code: 'код конца', day_code: 'тариф', note: 'примечание', reviewed: 'проверка', period: 'период', month_lock: 'месяц' };
// Буквы кодов, набранные в латинской раскладке (та же клавиша) или похожей латинской буквой.
const LATIN_CODE = { j: 'О', e: 'У', r: 'К', ',': 'Б', '<': 'Б', a: 'Ф', u: 'Г', g: 'П', k: 'К', o: 'О', b: 'Б', f: 'Ф', p: 'П' };

const hm = (m) => (m == null ? '' : `${Math.floor(m / 60)}:${String(m % 60).padStart(2, '0')}`);
/** Длительность в минутах → [ч]:мм; секунды отбрасываются, как в формате Excel. */
const dur = (min) => {
  const t = Math.floor(min + 1e-6);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
};
const durz = (min) => (min > 1e-6 ? dur(min) : '');
const pct = (x) => (x * 100).toFixed(1).replace('.', ',') + '%';
const num = (x) => String(Math.round(x * 100) / 100).replace('.', ',');
const dm = (iso) => `${iso.slice(8, 10)}.${iso.slice(5, 7)}`;
const dmy = (iso) => `${iso.slice(8, 10)}.${iso.slice(5, 7)}.${iso.slice(0, 4)}`;
const codeNorm = (v) => [...String(v || '').trim()].map((ch) => LATIN_CODE[ch.toLowerCase()] || ch).join('').toUpperCase();
const codeClass = (c) => (c ? ` code-${esc(c[0])}` : '');
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

const S = {
  meta: {}, ym: '', tab: 'timelist', view: null,
  sel: null, detail: null, detailKey: '', editing: false,
  compact: localStorage.getItem('tl_compact') === '1',
  sideOpen: localStorage.getItem('tl_side') !== '0',
  user: localStorage.getItem('tl_user') || '',
  employees: [], codes: [], settings: null,
  tabelEmp: null, messages: null, msgFilter: { employee: '', q: '', only: '' },
  periods: null, calendar: null, tgUsers: null,
};

async function api(method, path, body, raw) {
  const opt = { method, headers: { 'X-Timelist-User': encodeURIComponent(S.user) } };
  if (body !== undefined) {
    opt.body = raw ? body : JSON.stringify(body);
    opt.headers['Content-Type'] = raw ? 'application/octet-stream' : 'application/json';
  }
  const res = await fetch('/api' + path, opt);
  if (res.status === 401 && path !== '/login') {
    showLogin();
    throw new Error('Требуется вход');
  }
  const data = (res.headers.get('content-type') || '').includes('json') ? await res.json() : null;
  if (!res.ok) throw new Error((data && data.error) || `Ошибка ${res.status}`);
  return data;
}

function toast(text, isError) {
  const el = document.createElement('div');
  el.className = 'toast' + (isError ? ' err' : '');
  el.textContent = text.charAt(0).toUpperCase() + text.slice(1);
  $('#toasts').append(el);
  setTimeout(() => el.remove(), isError ? 6000 : 2600);
}

/** Выполняет действие и показывает ошибку, если оно не удалось. */
async function run(fn) {
  try {
    return await fn();
  } catch (e) {
    toast(e.message || String(e), true);
    return undefined;
  }
}

function openModal(title, html, wide) {
  $('#modal-title').textContent = title;
  $('#modal-body').innerHTML = html;
  $('#modal-body').onclick = null;
  $('#modal .modal').classList.toggle('wide', !!wide);
  $('#modal').hidden = false;
  const first = $('#modal-body input, #modal-body select, #modal-body textarea');
  if (first) first.focus();
  return $('#modal-body');
}
const closeModal = () => { $('#modal').hidden = true; };

function showLogin() {
  $('#login').hidden = false;
  $('#login-password').focus();
}

const employeeById = (id) => S.employees.find((e) => e.id === id);
const nickOf = (id) => (employeeById(id) || {}).nick || '—';
const absenceCodes = () => S.codes.filter((c) => c.kind === 'absence');
const tripCodes = () => S.codes.filter((c) => c.kind === 'trip');
const codeTitle = (code) => (S.codes.find((c) => c.code === code) || {}).title || code;

// ------------------------------------------------------------------ загрузка

async function loadRefs() {
  [S.employees, S.codes, S.settings] = await Promise.all([api('GET', '/employees'), api('GET', '/codes'), api('GET', '/settings')]);
}

async function loadMonth() {
  S.view = await api('GET', `/month/${S.ym}`);
  if (S.sel && (!S.view.employees[S.sel.e] || !S.view.days[S.sel.d])) S.sel = null;
}

async function loadMeta() {
  S.meta = await (await fetch('/api/meta')).json();
  const bot = S.meta.bot || {};
  const el = $('#bot-state');
  el.className = 'bot-state' + (bot.enabled ? (bot.last_error ? ' err' : ' on') : '');
  el.title = !bot.enabled
    ? 'Бот выключен: не задан TELEGRAM_BOT_TOKEN'
    : bot.last_error
      ? `Ошибка связи с Telegram: ${bot.last_error}`
      : `Бот @${bot.username || '…'} на связи${bot.last_ok ? ', опрос ' + bot.last_ok.slice(11, 16) : ''}`;
  $('span', el).textContent = bot.enabled ? (bot.username ? '@' + bot.username : 'бот') : 'бот выкл.';
}

function setHash() {
  history.replaceState(null, '', `#${S.tab}/${S.ym}`);
}

function shiftMonth(delta) {
  let [y, m] = S.ym.split('-').map(Number);
  m += delta;
  if (m < 1) { m = 12; y -= 1; }
  if (m > 12) { m = 1; y += 1; }
  return goMonth(`${y}-${String(m).padStart(2, '0')}`);
}

async function goMonth(ym) {
  if (!/^\d{4}-\d{2}$/.test(ym)) return;
  S.ym = ym;
  S.sel = null;
  S.detail = null;
  S.detailKey = '';
  S.messages = null;
  S.periods = null;
  S.calendar = null;
  setHash();
  await run(async () => {
    await loadMonth();
    render();
  });
}

/** Перечитывает данные после изменения, пришедшего от бота или из другого окна. */
async function reload() {
  const active = document.activeElement;
  const typing = active && /INPUT|TEXTAREA|SELECT/.test(active.tagName) && active.closest('#main, .modal');
  if (S.editing || typing || !$('#modal').hidden) {
    scheduleReload();
    return;
  }
  await run(async () => {
    const snapshot = () => JSON.stringify([S.view, S.employees, S.codes, S.settings]);
    const before = snapshot();
    const locked = !!(S.view && S.view.lock);
    await Promise.all([loadMonth(), loadRefs(), loadMeta()]);
    S.messages = null;
    S.periods = null;
    S.tgUsers = null;
    // свои же правки возвращаются тем же событием — перерисовывать нечего
    if (snapshot() === before && S.tab !== 'telegram') return;
    S.detailKey = '';
    if (S.tab === 'timelist' && $('#grid') && locked === !!S.view.lock) refreshGrid(false);
    else render(true);
  });
}
const scheduleReload = debounce(reload, 500);

// ------------------------------------------------------------------ каркас

function render(keepScroll) {
  const v = S.view;
  $('#month-title').textContent = v ? v.title : '…';
  $('#month-input').value = S.ym;
  $('#lock-chip').hidden = !(v && v.lock);
  if (v && v.lock) $('#lock-chip').title = `Закрыт ${v.lock.locked_at}${v.lock.locked_by ? ' · ' + v.lock.locked_by : ''}`;
  $('#user-btn').textContent = S.user || 'Указать имя';
  for (const b of $$('#tabs button')) {
    b.classList.toggle('active', b.dataset.tab === S.tab);
    if (b.dataset.tab === 'timelist') b.innerHTML = 'Тайм-лист' + (v && v.issues ? ` <span class="badge">${v.issues}</span>` : '');
  }
  const main = $('#main');
  const scroller = keepScroll && $('.grid-wrap, .page', main);
  const pos = scroller ? [scroller.scrollLeft, scroller.scrollTop] : null;
  ({ timelist: renderTimelist, tabel: renderTabel, svodka: renderSvodka, telegram: renderTelegram, periods: renderPeriods, refs: renderRefs }[S.tab] || renderTimelist)(main);
  const again = pos && $('.grid-wrap, .page', main);
  if (again) [again.scrollLeft, again.scrollTop] = pos;
}

function emptyState(main) {
  main.innerHTML = `<div class="page narrow"><div class="card"><div class="empty">
    <b>Сотрудников пока нет</b>
    Добавьте их в разделе «Справочники» или импортируйте прежнюю книгу Excel — оттуда же подтянутся отметки и сообщения.
    <div style="margin-top:14px"><button class="btn primary" data-go="refs">Открыть справочники</button></div>
  </div></div></div>`;
}

// ------------------------------------------------------------------ тайм-лист

function cellClasses(v, emp, day, cell, fi) {
  const f = FIELDS[fi];
  const value = cell.m[f];
  const cls = [isTime(f) ? 't' : 'k'];
  if (fi === 0) cls.push('g0');
  const manual = (cell.mask >> fi) & 1;
  const flags = cell.flags || [];
  if (value != null) cls.push(manual ? 'manual' : 'auto');
  if (!manual && f === 'end' && flags.includes('end_assumed') && !cell.reviewed) cls.push('assumed');
  if (!manual && f === 'start_code' && value && (flags.includes('code_guessed') || flags.includes('late_no_show')) && !cell.reviewed) cls.push('guess');
  // как условное форматирование в книге: есть начало без конца или конец без начала
  const closed = day.date < v.today;
  if (f === 'start' && cell.m.end && !cell.m.start) cls.push('err');
  if (f === 'end' && cell.m.start && !cell.m.end && closed) cls.push('err');
  if (!isTime(f) && value) cls.push('code-' + value[0]);
  if (f === 'day_code' && cell.note) cls.push('has-note');
  return cls.join(' ');
}

function rowClass(v, day) {
  const cls = [];
  if (day.kind === 'weekend') cls.push('weekend');
  if (day.kind === 'holiday') cls.push('holiday');
  if (day.kind === 'short') cls.push('short');
  if (day.date === v.today) cls.push('today');
  if (day.date > v.today) cls.push('future');
  return cls.join(' ');
}

function gridHtml(v) {
  const span = S.compact ? 1 : 7;
  let h = '<table class="grid"><thead><tr><th class="sl sl0" rowspan="2">№</th><th class="sl sl1" rowspan="2">Д.н.</th><th class="sl sl2" rowspan="2">Дата</th>';
  v.employees.forEach((e, ei) => {
    h += `<th class="emp" colspan="${span}" data-emp="${ei}" title="${esc(e.full_name)} — открыть табель"><span class="nick">${esc(e.nick)}</span><span class="pct">${pct(e.tabel.usage)}</span>${e.issues ? ` <span class="badge">${e.issues}</span>` : ''}</th>`;
  });
  h += '</tr><tr>';
  v.employees.forEach(() => {
    h += S.compact
      ? '<th class="g0">Начало – Конец</th>'
      : '<th class="g0" colspan="2">Начало</th><th colspan="2">Уходы</th><th colspan="2">Конец</th><th>Код</th>';
  });
  h += '</tr></thead><tbody>';
  v.days.forEach((day, di) => {
    h += `<tr class="${rowClass(v, day)}" data-day="${di}"><td class="sl sl0">${di + 1}</td><td class="sl sl1">${day.dow}</td><td class="sl sl2" title="${esc(day.title)}">${dm(day.date)}</td>`;
    v.employees.forEach((e, ei) => {
      const c = e.cells[di];
      if (S.compact) {
        const m = c.m;
        const times = m.start || m.end ? `${hm(m.start) || '…'}–${hm(m.end) || '…'}` : '';
        const codes = [m.start_code, m.away_code, m.end_code, m.day_code].filter(Boolean);
        const whole = !times && m.start_code ? `<span class="${codeClass(m.start_code).trim()}">${esc(codeTitle(m.start_code))}</span>` : '';
        const cls = ['compact', 'g0'];
        if ((c.issues || []).some((i) => i.severity === 'error')) cls.push('err');
        else if ((c.issues || []).length) cls.push('assumed');
        else if (c.mask) cls.push('manual');
        else if (times || codes.length) cls.push('auto');
        h += `<td class="${cls.join(' ')}" data-e="${ei}" data-f="0">${times}${whole}${
          !whole && codes.length ? `<span class="codes">${codes.map((x) => `<span class="${codeClass(x).trim()}">${esc(x)}</span>`).join(' ')}</span>` : ''
        }</td>`;
      } else {
        FIELDS.forEach((f, fi) => {
          const value = c.m[f];
          h += `<td class="${cellClasses(v, e, day, c, fi)}" data-e="${ei}" data-f="${fi}">${isTime(f) ? hm(value) : esc(value || '')}</td>`;
        });
      }
    });
    h += '</tr>';
  });
  h += '</tbody><tfoot>';
  const foot = [
    ['Отработано', 'Учётное время по тайм-листу: Конец − Начало − Уходы − обед', (e) => dur(e.tabel.worked)],
    ['Расчётное', 'Расчётное время с компенсацией по кодам причин', (e) => dur(e.tabel.calc)],
    ['% нормы', `Процент использования нормы рабочего времени (${num(v.norm_hours)} ч)`, (e) => pct(e.tabel.usage)],
    ['Опоздания', 'Количество и суммарное время опозданий, идущих в табель', (e) => (e.tabel.late_n_calc ? `${e.tabel.late_n_calc} · ${dur(e.tabel.late_t_calc)}` : '—')],
  ];
  for (const [label, hint, fn] of foot) {
    h += `<tr title="${esc(hint)}"><td class="sl lbl" colspan="3">${label}</td>${v.employees.map((e) => `<td class="g0" colspan="${span}">${fn(e)}</td>`).join('')}</tr>`;
  }
  return h + '</tfoot></table>';
}

function renderTimelist(main) {
  const v = S.view;
  if (!v) return;
  if (!v.employees.length) return emptyState(main);
  const errors = v.employees.reduce((n, e) => n + e.cells.reduce((k, c) => k + (c.issues || []).filter((i) => i.severity === 'error').length, 0), 0);
  main.innerHTML = `
    <div class="toolbar">
      <div class="seg" id="view-seg" title="Вид таблицы">
        <button data-compact="0" class="${S.compact ? '' : 'active'}">Как в Excel</button>
        <button data-compact="1" class="${S.compact ? 'active' : ''}">Компактно</button>
      </div>
      <button class="btn" id="issues-btn" ${v.issues ? '' : 'disabled'}>На проверку <span class="badge${errors ? ' err' : v.issues ? '' : ' ok'}">${v.issues}</span></button>
      <button class="btn" id="period-btn">+ Отпуск, больничный, командировка</button>
      <div class="legend">
        <span><span class="dot" style="background:var(--auto)"></span>из Telegram</span>
        <span><span class="dot" style="background:var(--manual)"></span><b>вручную</b></span>
        <span><span class="dot" style="background:var(--warn-bg);border:1px solid var(--warn-text)"></span>проверить</span>
        <span><span class="dot" style="background:var(--err-bg);border:1px solid var(--err-text)"></span>ошибка</span>
      </div>
      <span class="spacer"></span>
      <a class="btn" href="/api/export/${S.ym}" download>Excel</a>
      <button class="btn" id="lock-btn">${v.lock ? 'Открыть месяц' : 'Закрыть месяц'}</button>
      <button class="icon-btn" id="side-btn" title="Панель дня">${S.sideOpen ? '⟩' : '⟨'}</button>
    </div>
    <div class="tl-layout">
      <div class="grid-wrap" id="grid" tabindex="0">${gridHtml(v)}</div>
      <aside class="side${S.sideOpen ? '' : ' closed'}" id="side"></aside>
    </div>`;
  paintSelection();
  renderSide();
}

function selectedCell() {
  if (!S.sel || !S.view) return null;
  const emp = S.view.employees[S.sel.e];
  const day = S.view.days[S.sel.d];
  return emp && day ? { emp, day, cell: emp.cells[S.sel.d], field: FIELDS[S.sel.f] } : null;
}

function cellEl(sel) {
  return $(`#grid tr[data-day="${sel.d}"] td[data-e="${sel.e}"][data-f="${S.compact ? 0 : sel.f}"]`);
}

function paintSelection(scroll = true) {
  for (const el of $$('#grid td.sel')) el.classList.remove('sel');
  if (!S.sel) return;
  const td = cellEl(S.sel);
  if (!td) return;
  td.classList.add('sel');
  if (!scroll) return;
  // прокрутка с учётом закреплённых шапки, колонок дат и итогов
  const wrap = $('#grid');
  const r = td.getBoundingClientRect();
  const w = wrap.getBoundingClientRect();
  const pad = { left: 142, top: 60, bottom: 102, right: 4 };
  if (r.left < w.left + pad.left) wrap.scrollLeft -= w.left + pad.left - r.left;
  else if (r.right > w.left + wrap.clientWidth - pad.right) wrap.scrollLeft += r.right - (w.left + wrap.clientWidth - pad.right);
  if (r.top < w.top + pad.top) wrap.scrollTop -= w.top + pad.top - r.top;
  else if (r.bottom > w.top + wrap.clientHeight - pad.bottom) wrap.scrollTop += r.bottom - (w.top + wrap.clientHeight - pad.bottom);
}

function select(e, d, f, focus = true) {
  const v = S.view;
  if (!v) return;
  e = Math.max(0, Math.min(v.employees.length - 1, e));
  d = Math.max(0, Math.min(v.days.length - 1, d));
  f = Math.max(0, Math.min(FIELDS.length - 1, f));
  S.sel = { e, d, f };
  paintSelection();
  renderSide();
  if (focus && $('#grid')) $('#grid').focus({ preventScroll: true });
}

function move(dx, dy) {
  if (!S.sel) return select(0, Math.max(0, S.view.days.findIndex((x) => x.date === S.view.today)), 0);
  const step = S.compact ? FIELDS.length : 1;
  const linear = Math.max(0, Math.min(S.view.employees.length * FIELDS.length - 1, S.sel.e * FIELDS.length + S.sel.f + dx * step));
  select(Math.floor(linear / FIELDS.length), S.sel.d + dy, S.compact ? 0 : linear % FIELDS.length);
}

async function saveMark(emp, date, edit) {
  const ok = await run(async () => {
    S.view = await api('PUT', `/marks/${emp.id}/${date}`, edit);
    S.detailKey = '';
    return true;
  });
  if (S.tab === 'timelist' && $('#grid')) refreshGrid(true);
  return ok;
}

/** Обновляет только сетку и панель дня, не трогая панель инструментов и прокрутку. */
function refreshGrid(scrollToSelection) {
  const wrap = $('#grid');
  const pos = [wrap.scrollLeft, wrap.scrollTop];
  wrap.innerHTML = gridHtml(S.view);
  [wrap.scrollLeft, wrap.scrollTop] = pos;
  paintSelection(scrollToSelection);
  renderSide(true);
  render.badge();
}
render.badge = () => {
  const b = $('#tabs button[data-tab="timelist"]');
  b.innerHTML = 'Тайм-лист' + (S.view.issues ? ` <span class="badge">${S.view.issues}</span>` : '');
  const btn = $('#issues-btn');
  if (btn) {
    btn.disabled = !S.view.issues;
    $('.badge', btn).textContent = S.view.issues;
  }
};

function startEdit(initial) {
  const cur = selectedCell();
  if (!cur || S.editing) return;
  if (S.view.lock) return toast('Месяц закрыт — откройте его, чтобы вносить правки', true);
  if (S.compact) {
    if (!S.sideOpen) toggleSide();
    const input = $('#side input[data-field="start"]');
    if (input) input.focus();
    return;
  }
  const td = cellEl(S.sel);
  const wrap = $('#grid');
  const r = td.getBoundingClientRect();
  const w = wrap.getBoundingClientRect();
  const input = document.createElement('input');
  input.className = 'cell-editor';
  input.style.left = `${r.left - w.left + wrap.scrollLeft}px`;
  input.style.top = `${r.top - w.top + wrap.scrollTop}px`;
  input.style.width = `${Math.max(r.width, isTime(cur.field) ? 56 : 40)}px`;
  input.style.height = `${r.height}px`;
  input.placeholder = isTime(cur.field) ? 'ч:мм' : '';
  input.value = initial != null ? initial : td.textContent;
  wrap.append(input);
  S.editing = true;
  input.focus();
  if (initial == null) input.select();

  let done = false;
  const finish = async (commit, dx = 0, dy = 0) => {
    if (done) return;
    done = true;
    const raw = input.value.trim();
    const value = isTime(cur.field) ? raw : codeNorm(raw);
    const before = isTime(cur.field) ? hm(cur.cell.m[cur.field]) : cur.cell.m[cur.field] || '';
    let ok = true;
    if (commit && value !== before) ok = await saveMark(cur.emp, cur.day.date, { set: { [cur.field]: value || null } });
    S.editing = false;
    input.remove();
    if (ok && (dx || dy)) move(dx, dy);
    if ($('#grid')) $('#grid').focus({ preventScroll: true });
  };
  input.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') { ev.preventDefault(); finish(true, 0, 1); }
    else if (ev.key === 'Escape') { ev.preventDefault(); finish(false); }
    else if (ev.key === 'Tab') { ev.preventDefault(); finish(true, ev.shiftKey ? -1 : 1, 0); }
    else if (ev.key === 'ArrowDown') { ev.preventDefault(); finish(true, 0, 1); }
    else if (ev.key === 'ArrowUp') { ev.preventDefault(); finish(true, 0, -1); }
    ev.stopPropagation();
  });
  input.addEventListener('blur', () => finish(true));
}

function clearSelected() {
  const cur = selectedCell();
  if (!cur || S.compact) return;
  if (S.view.lock) return toast('Месяц закрыт — откройте его, чтобы вносить правки', true);
  const manual = (cur.cell.mask >> S.sel.f) & 1;
  if (cur.cell.m[cur.field] != null) saveMark(cur.emp, cur.day.date, { set: { [cur.field]: null } });
  else if (manual) saveMark(cur.emp, cur.day.date, { reset: [cur.field] });
}

function onGridKey(ev) {
  if (S.editing || ev.target.closest('.cell-editor') || ev.ctrlKey || ev.metaKey || ev.altKey) return;
  const k = ev.key;
  const moves = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
  if (moves[k]) { ev.preventDefault(); move(...moves[k]); }
  else if (k === 'Tab') { ev.preventDefault(); move(ev.shiftKey ? -1 : 1, 0); }
  else if (k === 'Enter' || k === 'F2') { ev.preventDefault(); startEdit(null); }
  else if (k === 'Delete' || k === 'Backspace') { ev.preventDefault(); clearSelected(); }
  else if (k === 'Home') { ev.preventDefault(); if (S.sel) select(S.sel.e, 0, S.sel.f); }
  else if (k === 'End') { ev.preventDefault(); if (S.sel) select(S.sel.e, S.view.days.length - 1, S.sel.f); }
  else if (k.length === 1 && k !== ' ' && S.sel) { ev.preventDefault(); startEdit(k); }
}

function toggleSide() {
  S.sideOpen = !S.sideOpen;
  localStorage.setItem('tl_side', S.sideOpen ? '1' : '0');
  $('#side').classList.toggle('closed', !S.sideOpen);
  $('#side-btn').textContent = S.sideOpen ? '⟩' : '⟨';
  renderSide();
}

// ----- панель дня

const loadDetail = debounce(async (empId, date, key) => {
  try {
    const detail = await api('GET', `/day/${empId}/${date}`);
    if (S.detailKey === key) {
      S.detail = detail;
      renderSide(true);
    }
  } catch (e) { /* панель остаётся без сообщений, отметки видны и так */ }
}, 120);

function issueActions(issue, cur) {
  const m = cur.cell.m;
  const absence = absenceCodes();
  const codeBtns = (field) => absence.map((c) => `<button class="btn small" data-set-field="${field}" data-set-value="${esc(c.code)}" title="${esc(c.title)}"><span class="${codeClass(c.code).trim()}">${esc(c.code)}</span></button>`).join('');
  switch (issue.kind) {
    case 'end_assumed':
      return `<button class="btn small" data-set-field="end" data-set-value="${hm(m.end)}">Подтвердить ${hm(m.end)}</button><button class="btn small" data-focus-field="end">Указать время</button>`;
    case 'no_end':
      return `<button class="btn small" data-set-field="end" data-set-value="${hm(S.view.rules.work_end)}">Поставить ${hm(S.view.rules.work_end)}</button><button class="btn small" data-focus-field="end">Указать время</button>`;
    case 'no_start':
      return `<button class="btn small" data-set-field="start" data-set-value="${hm(S.view.rules.work_start)}">Поставить ${hm(S.view.rules.work_start)}</button><button class="btn small" data-focus-field="start">Указать время</button>`;
    case 'code_guessed':
    case 'late_no_show':
      return `<button class="btn small" data-set-field="start_code" data-set-value="${esc(m.start_code || '')}">Подтвердить «${esc(m.start_code || '')}»</button>${codeBtns('start_code')}`;
    case 'no_marks':
      return `${codeBtns('start_code')}<button class="btn small" data-review="1">Так и должно быть</button>`;
    case 'unrecognized':
      return '<button class="btn small" data-review="1">Отметить день проверенным</button>';
    default:
      return '';
  }
}

function renderSide(keepFocus) {
  const side = $('#side');
  if (!side || !S.sideOpen) return;
  const cur = selectedCell();
  if (!cur) {
    side.innerHTML = `<div class="empty"><b>Выберите ячейку</b>Здесь появятся сообщения сотрудника за день, расчёт и история правок.<br><br>
      <span class="hint">Стрелки — перемещение, Enter или ввод с клавиатуры — правка, Delete — очистить. Время можно вводить как 930, 9.30 или 9:30.</span></div>`;
    return;
  }
  const { emp, day, cell } = cur;
  const key = `${emp.id}/${day.date}`;
  if (S.detailKey !== key) {
    S.detailKey = key;
    S.detail = null;
    loadDetail(emp.id, day.date, key);
  }
  const focused = keepFocus && document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.field : null;
  const locked = !!S.view.lock;
  const d = S.detail;
  const kindChip = { weekend: 'выходной', holiday: day.title || 'праздник', short: 'сокращённый день', work: '' }[day.kind];
  const fieldInput = (f, fi) => {
    const manual = (cell.mask >> fi) & 1;
    const value = isTime(f) ? hm(cell.m[f]) : cell.m[f] || '';
    const autoValue = d ? (isTime(f) ? hm(d.auto[f]) : d.auto[f] || '') : null;
    const hint = manual && autoValue != null && autoValue !== value ? `<span class="auto-hint" data-reset="${f}" title="Вернуть значение автоматики">↺ авто: ${esc(autoValue || 'пусто')}</span>` : '';
    return `<label class="field" title="${esc(FIELD_HINT[f])}">${isTime(f) || f === 'day_code' ? FIELD_LABEL[f] : 'код'} ${hint}
      <input data-field="${f}" value="${esc(value)}" ${locked ? 'disabled' : ''} placeholder="${isTime(f) ? 'ч:мм' : '—'}" ${isTime(f) ? 'inputmode="numeric"' : `list="codes-${f === 'day_code' ? 'trip' : 'absence'}" maxlength="3"`}></label>`;
  };
  const c = cell.calc;
  const issues = cell.issues || [];
  side.innerHTML = `
    <div class="side-head">
      <div class="who"><b>${esc(emp.nick)} · ${day.dow} ${dmy(day.date)}</b><span class="muted">${esc(emp.full_name)}</span></div>
      ${kindChip ? `<span class="chip gray">${esc(kindChip)}</span>` : ''}
    </div>
    ${issues.length ? `<section>${issues.map((i) => `<div class="issue ${i.severity}"><div>${esc(i.text)}</div>${locked ? '' : `<div class="acts">${issueActions(i, cur)}</div>`}</div>`).join('')}</section>` : ''}
    <section>
      <h4>Отметка дня ${cell.reviewed ? '<span class="chip gray">проверено</span>' : ''}</h4>
      <div class="day-form">
        ${fieldInput('start', 0)}${fieldInput('start_code', 1)}
        ${fieldInput('away', 2)}${fieldInput('away_code', 3)}
        ${fieldInput('end', 4)}${fieldInput('end_code', 5)}
        <div class="full">${fieldInput('day_code', 6)}</div>
        <label class="field full">Примечание<input data-note value="${esc(cell.note || '')}" ${locked ? 'disabled' : ''} placeholder="Комментарий к дню"></label>
      </div>
      <datalist id="codes-absence">${absenceCodes().map((x) => `<option value="${esc(x.code)}">${esc(x.title)}</option>`).join('')}</datalist>
      <datalist id="codes-trip">${tripCodes().map((x) => `<option value="${esc(x.code)}">${esc(x.title)}</option>`).join('')}</datalist>
      ${locked ? '' : `<div class="row" style="margin-top:10px">
        ${cell.mask ? '<button class="btn small" data-reset="all" title="Убрать ручные правки и вернуть отметку из сообщений">↺ Вернуть авто</button>' : ''}
        ${cell.reviewed ? '<button class="btn small" data-review="0">Снять «проверено»</button>' : ''}
      </div>`}
    </section>
    <section>
      <h4>Расчёт дня</h4>
      <div class="kv">
        <span></span><span class="h">т-лист</span><span class="h">расч.</span>
        <span>Отработано</span><span class="v">${dur(c.worked)}</span><span class="v"><b>${dur(c.calc)}</b></span>
        <span>Опоздание</span><span class="v">${durz(c.late) || '—'}</span><span class="v">${durz(c.late_calc) || '—'}</span>
        <span>Отлучки</span><span class="v">${durz(c.away) || '—'}</span><span class="v">${durz(c.away_calc) || '—'}</span>
        <span>Ранний уход</span><span class="v">${durz(c.early) || '—'}</span><span class="v">${durz(c.early_calc) || '—'}</span>
      </div>
    </section>
    <section>
      <h4>Сообщения Telegram ${d ? `<span class="chip gray">${d.messages.length}</span>` : ''}</h4>
      ${!d ? '<span class="muted">Загрузка…</span>' : d.messages.length ? d.messages.map((m) => messageHtml(m, emp.id, day.date)).join('') : '<span class="muted">За этот день сообщений нет.</span>'}
      ${locked ? '' : '<div class="row" style="margin-top:8px"><button class="btn small" id="add-msg-btn">+ Добавить сообщение</button></div>'}
    </section>
    ${d && d.periods.length ? `<section><h4>Период</h4>${d.periods.map((p) => `<div><b class="${codeClass(p.code).trim()}">${esc(p.code)}</b> ${esc(codeTitle(p.code))}${p.tariff ? ' (тариф К1–К3)' : ''}: ${dmy(p.date_from)} — ${dmy(p.date_to)} ${p.note ? `<span class="muted">· ${esc(p.note)}</span>` : ''}</div>`).join('')}</section>` : ''}
    <section>
      <h4>История изменений</h4>
      <div class="audit">${d && d.audit.length ? d.audit.map(auditHtml).join('') : '<span>Правок не было.</span>'}</div>
    </section>`;
  if (focused) {
    const el = $(`#side input[data-field="${focused}"]`);
    if (el) el.focus();
  }
}

function messageHtml(m, empId, date) {
  const own = m.author_id === empId;
  const other = !own ? ` · от ${esc(m.author_id ? nickOf(m.author_id) : m.sender_name)}` : '';
  const elsewhere = m.targets.length && !m.targets.some((t) => t.date === date) ? ` · относится к ${dm(m.targets[0].date)}` : '';
  const backdated = m.sent_at.slice(0, 10) !== date ? ` · отправлено ${dm(m.sent_at)}` : elsewhere;
  const chip = m.recognized ? (m.kind === 'info' ? 'gray' : '') : 'warn';
  return `<div class="msg${m.ignored ? ' ignored' : ''}">
    <div class="meta"><b>${m.sent_at.slice(11, 16)}</b><span class="chip ${chip}">${esc(m.summary)}</span><span>${other}${backdated}</span>
      <button class="link" data-ignore="${m.id}" data-value="${m.ignored ? 0 : 1}">${m.ignored ? 'учитывать' : 'не учитывать'}</button></div>
    <div class="text">${esc(m.text)}</div></div>`;
}

function auditHtml(a) {
  const change = a.field === 'period' || a.field === 'month_lock' ? esc(a.new_value || `удалён: ${a.old_value}`) : `${esc(a.old_value || 'пусто')} → <b>${esc(a.new_value || 'пусто')}</b>`;
  return `<div>${esc(a.at.slice(8, 10))}.${esc(a.at.slice(5, 7))} ${esc(a.at.slice(11, 16))} · ${esc(a.user || 'без имени')} · ${esc(AUDIT_LABEL[a.field] || a.field)}: ${change}</div>`;
}

async function onSideChange(ev) {
  const cur = selectedCell();
  if (!cur) return;
  const input = ev.target;
  if (input.dataset.field) {
    const f = input.dataset.field;
    const value = isTime(f) ? input.value.trim() : codeNorm(input.value);
    const before = isTime(f) ? hm(cur.cell.m[f]) : cur.cell.m[f] || '';
    if (value !== before) await saveMark(cur.emp, cur.day.date, { set: { [f]: value || null } });
  } else if ('note' in input.dataset) {
    if (input.value.trim() !== (cur.cell.note || '')) await saveMark(cur.emp, cur.day.date, { note: input.value });
  }
}

async function onSideClick(ev) {
  const cur = selectedCell();
  const t = ev.target.closest('[data-set-field], [data-focus-field], [data-reset], [data-review], [data-ignore], #add-msg-btn');
  if (!cur || !t) return;
  if (t.dataset.setField) await saveMark(cur.emp, cur.day.date, { set: { [t.dataset.setField]: t.dataset.setValue || null } });
  else if (t.dataset.focusField) $(`#side input[data-field="${t.dataset.focusField}"]`).focus();
  else if (t.dataset.reset) await saveMark(cur.emp, cur.day.date, { reset: [t.dataset.reset] });
  else if (t.dataset.review) await saveMark(cur.emp, cur.day.date, { reviewed: t.dataset.review === '1' });
  else if (t.dataset.ignore) {
    await run(() => api('PATCH', `/messages/${t.dataset.ignore}`, { ignored: t.dataset.value === '1' }));
    await reloadNow();
  } else if (t.id === 'add-msg-btn') addMessageDialog(cur.emp.id, cur.day.date);
}

async function reloadNow() {
  await run(async () => {
    await loadMonth();
    S.messages = null;
    S.detailKey = '';
    render(true);
  });
}

function addMessageDialog(empId, date) {
  const body = openModal('Добавить сообщение', `
    <p class="hint">Пригодится, если сотрудник сообщил об отметке устно или в личной переписке. Сообщение разбирается так же, как из группы.</p>
    <div class="form-grid">
      <label class="field">Сотрудник<select name="employee">${S.employees.map((e) => `<option value="${e.id}" ${e.id === empId ? 'selected' : ''}>${esc(e.nick)} — ${esc(e.full_name)}</option>`).join('')}</select></label>
      <label class="field">Дата<input type="date" name="date" value="${date}"></label>
      <label class="field">Время<input name="time" value="9:00" placeholder="ч:мм"></label>
      <label class="field full">Текст<input name="text" placeholder="Например: офис, до банк"></label>
    </div>
    <div class="actions"><button class="btn" data-close>Отмена</button><button class="btn primary" data-ok>Добавить</button></div>`);
  $('[data-ok]', body).onclick = async () => {
    const v = (n) => $(`[name="${n}"]`, body).value;
    const ok = await run(() => api('POST', '/messages', { employee_id: Number(v('employee')), date: v('date'), time: v('time'), text: v('text') }));
    if (ok) {
      closeModal();
      await reloadNow();
    }
  };
}

// ----- «на проверку»

function issuesDialog() {
  const v = S.view;
  const items = [];
  v.employees.forEach((e, ei) => e.cells.forEach((c, di) => (c.issues || []).forEach((i) => items.push({ e, ei, di, i }))));
  items.sort((a, b) => (a.i.severity === b.i.severity ? a.di - b.di || a.ei - b.ei : a.i.severity === 'error' ? -1 : 1));
  const body = openModal(`На проверку: ${items.length}`, `
    <p class="hint">Автоматика расставила отметки сама, но в этих днях сомневается. Нажмите на строку — откроется день с сообщениями и кнопками быстрого исправления.</p>
    <div class="issues-list">${items.map((x, n) => `<button data-n="${n}"><span class="when">${v.days[x.di].dow} ${dmy(v.days[x.di].date)}</span><span class="who">${esc(x.e.nick)}</span><span class="chip ${x.i.severity === 'error' ? 'err' : 'warn'}">${esc(x.i.text)}</span></button>`).join('')}</div>`, true);
  body.onclick = (ev) => {
    const b = ev.target.closest('button[data-n]');
    if (!b) return;
    const x = items[Number(b.dataset.n)];
    closeModal();
    if (!S.sideOpen) toggleSide();
    const field = { no_start: 0, end_assumed: 4, no_end: 4 }[x.i.kind] ?? 1;
    select(x.ei, x.di, field);
  };
}

// ----- период отсутствия

function periodDialog(preset = {}) {
  const cur = selectedCell();
  const date = preset.date || (cur ? cur.day.date : S.view.today.slice(0, 7) === S.ym ? S.view.today : `${S.ym}-01`);
  const picked = preset.employee ?? (cur ? cur.emp.id : null);
  const body = openModal('Отпуск, больничный, командировка', `
    <p class="hint">Код проставится на все рабочие дни периода автоматически. Если человек в эти дни всё-таки отметился в группе, его время сохранится.</p>
    <div class="form-grid">
      <label class="field full">Причина<select name="code">
        ${absenceCodes().map((c) => `<option value="${esc(c.code)}">${esc(c.code)} — ${esc(c.title)} (коэфф. ${num(c.coef)})</option>`).join('')}
        <option value="К*">К1–К3 — иногородняя командировка с тарифом</option>
      </select></label>
      <label class="field">С<input type="date" name="from" value="${date}"></label>
      <label class="field">По<input type="date" name="to" value="${date}"></label>
      <div class="full"><div class="field" style="margin-bottom:6px">Сотрудники</div><div class="checks">
        ${S.employees.filter((e) => e.active).map((e) => `<label class="check"><input type="checkbox" name="emp" value="${e.id}" ${e.id === picked ? 'checked' : ''}> ${esc(e.nick)}</label>`).join('')}
      </div></div>
      <label class="field full">Примечание<input name="note" placeholder="Например: приказ № 12, объект в Самарканде"></label>
    </div>
    <p class="hint" id="tariff-hint" hidden>Тариф расставится по дням: ${tripCodes().map((c) => `<b>${esc(c.code)}</b> ×${num(c.coef)}`).join(', ')} — первый и последний день (дорога), рабочие дни на объекте и выходные соответственно.</p>
    <div class="actions"><button class="btn" data-close>Отмена</button><button class="btn primary" data-ok>Сохранить</button></div>`);
  const codeSel = $('[name="code"]', body);
  if (preset.code) codeSel.value = preset.code;
  const sync = () => { $('#tariff-hint', body).hidden = codeSel.value !== 'К*'; };
  codeSel.onchange = sync;
  sync();
  $('[name="from"]', body).onchange = (ev) => {
    const to = $('[name="to"]', body);
    if (to.value < ev.target.value) to.value = ev.target.value;
  };
  $('[data-ok]', body).onclick = async () => {
    const tariff = codeSel.value === 'К*';
    const payload = {
      employee_ids: $$('[name="emp"]:checked', body).map((x) => Number(x.value)),
      code: tariff ? 'К' : codeSel.value,
      tariff,
      date_from: $('[name="from"]', body).value,
      date_to: $('[name="to"]', body).value,
      note: $('[name="note"]', body).value,
    };
    const ok = await run(() => api('POST', '/periods', payload));
    if (ok) {
      closeModal();
      toast('Период сохранён');
      S.periods = null;
      await reloadNow();
    }
  };
}

async function toggleLock() {
  const v = S.view;
  const question = v.lock
    ? `Открыть ${v.title}? Отметки снова будут пересчитываться по сообщениям и периодам.`
    : `Закрыть ${v.title}? Отметки будут зафиксированы: новые сообщения и правки периодов их уже не изменят.`;
  if (!confirm(question)) return;
  await run(async () => {
    S.view = await api(v.lock ? 'DELETE' : 'POST', `/months/${S.ym}/lock`);
    S.detailKey = '';
    render(true);
    toast(S.view.lock ? 'Месяц закрыт' : 'Месяц открыт');
  });
}

// ------------------------------------------------------------------ табель

function renderTabel(main) {
  const v = S.view;
  if (!v) return;
  if (!v.employees.length) return emptyState(main);
  if (!v.employees.some((e) => e.id === S.tabelEmp)) S.tabelEmp = v.employees[0].id;
  const emp = v.employees.find((e) => e.id === S.tabelEmp);
  const rows = [
    ['Процент испол. норм. раб. вр.', '', (t) => `<b>${pct(t.usage)}</b>`],
    ['Учётное время по Т-листу', '', (t) => dur(t.worked)],
    ['Расчётное время с компенс.', '', (t) => dur(t.calc)],
    ['Макс. время, с учёт. перераб.', '', (t) => `<b>${dur(t.max)}</b>`],
    ['Кол-во опозданий', 'т-лист', (t) => t.late_n || '—'],
    ['', 'расч.', (t) => t.late_n_calc || '—'],
    ['Время опозданий', 'т-лист', (t) => durz(t.late_t) || '—'],
    ['', 'расч.', (t) => durz(t.late_t_calc) || '—'],
    ['Кол-во отлучений', 'т-лист', (t) => t.away_n || '—'],
    ['', 'расч.', (t) => t.away_n_calc || '—'],
    ['Время отлучений', 'т-лист', (t) => durz(t.away_t) || '—'],
    ['', 'расч.', (t) => durz(t.away_t_calc) || '—'],
    ['Кол-во ранних уходов', 'т-лист', (t) => t.early_n || '—'],
    ['', 'расч.', (t) => t.early_n_calc || '—'],
    ['Время ранних уходов', 'т-лист', (t) => durz(t.early_t) || '—'],
    ['', 'расч.', (t) => durz(t.early_t_calc) || '—'],
  ];
  const cells = emp.cells;
  main.innerHTML = `<div class="page">
    <div class="card">
      <header><h3>Табель · ${esc(v.title)}</h3><span class="chip gray">Норма ${num(v.norm_hours)} ч / ${v.norm_days} дн.${v.norm_source === 'calendar' ? ' (расчётная)' : ''}</span>
        <span class="chip gray">День ${hm(v.rules.work_start)}–${hm(v.rules.work_end)}, обед ${hm(v.rules.lunch)}</span></header>
      <div class="body flush"><table class="tbl">
        <thead><tr><th>Показатель</th><th></th>${v.employees.map((e) => `<th class="num" title="${esc(e.full_name)}">${esc(e.nick)}</th>`).join('')}</tr></thead>
        <tbody>${rows.map(([label, sub, fn]) => `<tr class="${sub === 'расч.' ? 'sub' : ''}"><td>${label}</td><td class="muted">${sub}</td>${v.employees.map((e) => `<td class="num">${fn(e.tabel)}</td>`).join('')}</tr>`).join('')}</tbody>
      </table></div>
    </div>
    <div class="card">
      <header><h3>По дням</h3><div class="seg" id="tabel-emp">${v.employees.map((e) => `<button data-id="${e.id}" class="${e.id === emp.id ? 'active' : ''}" title="${esc(e.full_name)}">${esc(e.nick)}</button>`).join('')}</div></header>
      <div class="body flush"><table class="tbl">
        <thead>
          <tr><th rowspan="2">№</th><th rowspan="2">Дата</th><th rowspan="2">Д.нед.</th><th colspan="2" class="c">Отраб. врем.</th><th colspan="7" class="c">Т-лист</th><th colspan="2" class="c">Опоздания</th><th colspan="2" class="c">Отсутствия</th><th colspan="2" class="c">Ранний уход</th></tr>
          <tr><th class="num">т-лист</th><th class="num">расч.</th><th class="c">Начало</th><th class="c">код</th><th class="c">Уходы</th><th class="c">код</th><th class="c">Конец</th><th class="c">код</th><th class="c">Код</th>
            <th class="num">т-лист</th><th class="num">расч.</th><th class="num">т-лист</th><th class="num">расч.</th><th class="num">т-лист</th><th class="num">расч.</th></tr>
        </thead>
        <tbody>${v.days.map((day, i) => {
          const c = cells[i];
          const m = c.m;
          const x = c.calc;
          const code = (val) => `<td class="c${codeClass(val)}"><b>${esc(val || '')}</b></td>`;
          return `<tr style="${day.kind === 'weekend' || day.kind === 'holiday' ? 'color:var(--weekend-text)' : ''}">
            <td class="muted">${i + 1}</td><td>${dmy(day.date)}</td><td>${day.dow}</td>
            <td class="num">${durz(x.worked)}</td><td class="num"><b>${durz(x.calc)}</b></td>
            <td class="c">${hm(m.start)}</td>${code(m.start_code)}<td class="c">${hm(m.away)}</td>${code(m.away_code)}<td class="c">${hm(m.end)}</td>${code(m.end_code)}${code(m.day_code)}
            <td class="num">${durz(x.late)}</td><td class="num">${durz(x.late_calc)}</td><td class="num">${durz(x.away)}</td><td class="num">${durz(x.away_calc)}</td><td class="num">${durz(x.early)}</td><td class="num">${durz(x.early_calc)}</td></tr>`;
        }).join('')}
        <tr class="group"><td colspan="3">Итого · ${esc(emp.nick)}</td><td class="num">${dur(emp.tabel.worked)}</td><td class="num">${dur(emp.tabel.calc)}</td><td colspan="7"></td>
          <td class="num">${durz(emp.tabel.late_t)}</td><td class="num">${durz(emp.tabel.late_t_calc)}</td><td class="num">${durz(emp.tabel.away_t)}</td><td class="num">${durz(emp.tabel.away_t_calc)}</td><td class="num">${durz(emp.tabel.early_t)}</td><td class="num">${durz(emp.tabel.early_t_calc)}</td></tr>
        </tbody>
      </table></div>
    </div></div>`;
}

// ------------------------------------------------------------------ сводка

function barChart(items, fmt, good) {
  const n = items.length;
  const W = Math.max(300, n * 42);
  const H = 150;
  const top = 18;
  const base = H - 20;
  const max = Math.max(...items.map((i) => i.value), 1e-9);
  const bw = Math.min(26, (W - 16) / n - 8);
  let h = `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet" role="img"><line x1="4" x2="${W - 4}" y1="${base}" y2="${base}"/>`;
  items.forEach((it, i) => {
    const cx = 8 + ((W - 16) / n) * (i + 0.5);
    const bh = it.value > 0 ? Math.max(2, ((base - top) * it.value) / max) : 0;
    const hi = it.value > 0 && it.value === max;
    h += `<rect class="b${hi ? ' hi' : ''}${good ? ' good' : ''}" x="${cx - bw / 2}" y="${base - bh}" width="${bw}" height="${bh}" rx="3"><title>${esc(it.label)}: ${fmt(it.value)}</title></rect>`;
    if (it.value > 0) h += `<text class="v" x="${cx}" y="${base - bh - 4}" text-anchor="middle">${fmt(it.value)}</text>`;
    h += `<text x="${cx}" y="${H - 6}" text-anchor="middle">${esc(it.label)}</text>`;
  });
  return h + '</svg>';
}

function renderSvodka(main) {
  const v = S.view;
  if (!v) return;
  if (!v.employees.length) return emptyState(main);
  const E = v.employees;
  const series = (fn) => E.map((e) => ({ label: e.nick, value: fn(e.svodka) }));
  // подписи столбцов: длинные суммы показываются целыми часами, чтобы не налезали друг на друга
  const short = (min) => (min >= 6000 ? `${Math.round(min / 60)} ч` : dur(min));
  const monthOver = v.today > v.days[v.days.length - 1].date;
  const charts = [
    ['Процент использования рабочего времени', 'Относительные уровни за месяц', series((s) => s.usage), (x) => Math.round(x * 100) + '%', true],
    ['Не отработанное время по табелю', 'Суммарно пропущено за месяц', series((s) => s.missed_tab), short],
    ['Количество опозданий', 'Опоздания и задержки начала дня', series((s) => s.late_n), String],
    ['Время опозданий', 'Суммарно за месяц, в табель', series((s) => s.late_t), short],
    ['Количество отлучений', 'Отлучения из офиса в течение дня', series((s) => s.away_n), String],
    ['Время отлучений', 'Суммарно за месяц, в табель', series((s) => s.away_t), short],
    ['Количество ранних уходов', 'Уходы до конца рабочего дня', series((s) => s.early_n), String],
    ['Время сокращения рабочего дня', 'Из-за ранних уходов, в табель', series((s) => s.early_t), short],
  ];
  const rows = [
    ['1', 'Норматив рабочего времени (час)', (s) => dur(s.norm), true],
    ['2', 'Процент использования раб. времени', (s) => `<b>${pct(s.usage)}</b>`, true],
    ['2.1', 'Время, отмеченное в Тайм-листе', (s) => dur(s.tl_time)],
    ['2.2', 'Время, учитываемое в Табеле', (s) => dur(s.tab_time)],
    ['2.3', 'Всего пропущенного времени по Тайм-листу', (s) => durz(s.missed_tl) || '—'],
    ['2.4', 'Всего не отработанного времени по Табелю', (s) => durz(s.missed_tab) || '—'],
    ['2.5', 'Всего переработанного времени', (s) => durz(s.overtime) || '—'],
    ['3', 'Кол-во опозданий и задержек нач. раб. дня', (s) => s.late_n || '—', true],
    ['3.1', 'Всего в месяц время опозданий в табель', (s) => durz(s.late_t) || '—'],
    ['4', 'Кол-во отлучений из офиса в теч. раб. дня', (s) => s.away_n || '—', true],
    ['4.1', 'Всего в месяц время отлучений в табель', (s) => durz(s.away_t) || '—'],
    ['5', 'Кол-во ранних уходов до конца раб. дня', (s) => s.early_n || '—', true],
    ['5.1', 'Всего в месяц время сокращения раб. дня в табель', (s) => durz(s.early_t) || '—'],
  ];
  const avg = E.reduce((a, e) => a + e.svodka.ktu, 0) / E.length;
  main.innerHTML = `<div class="page">
    <div class="cards" style="grid-template-columns:minmax(320px,440px) 1fr">
      <div class="card">
        <header><h3>КТУ, % · ${esc(v.title)}</h3><span class="chip">в среднем ${pct(avg)}</span></header>
        <div class="body">
          ${E.map((e, i) => `<div class="ktu-row" title="${esc(e.full_name)}"><span class="muted">${i + 1}</span><b>${esc(e.nick)}</b>
            <div class="bar"><i class="${monthOver && e.svodka.ktu < 0.8 ? 'low' : ''}" style="width:${Math.min(100, (e.svodka.ktu / Math.max(v.limit, 1)) * 100)}%"></i></div><span class="val">${pct(e.svodka.ktu)}</span></div>`).join('')}
          <div class="row" style="margin-top:12px;align-items:center">
            <label class="field" title="Если процент использования рабочего времени больше лимита, в КТУ идёт лимит">Лимит раб. времени, %
              <input id="limit-input" type="number" min="1" step="1" value="${Math.round(v.limit * 1000) / 10}" style="width:110px" ${v.lock ? 'disabled' : ''}></label>
            <span class="hint" style="flex:1">Переработка сверх лимита в КТУ не засчитывается (ячейка E2 листа «Сводка»).</span>
          </div>
        </div>
      </div>
      <div class="card">
        <header><h3>Показатели месяца</h3><span class="chip gray">Норма ${num(v.norm_hours)} ч / ${v.norm_days} дн.</span></header>
        <div class="body flush"><table class="tbl">
          <thead><tr><th>№</th><th>Показатель</th>${E.map((e) => `<th class="num" title="${esc(e.full_name)}">${esc(e.nick)}</th>`).join('')}</tr></thead>
          <tbody>${rows.map(([no, label, fn, head]) => `<tr class="${head ? '' : 'sub'}"><td>${no}</td><td>${label}</td>${E.map((e) => `<td class="num">${fn(e.svodka)}</td>`).join('')}</tr>`).join('')}</tbody>
        </table></div>
      </div>
    </div>
    <div class="charts">${charts.map(([title, sub, items, fmt, good]) => `<div class="card chart"><h5>${title}</h5><span class="hint">${sub}</span>${barChart(items, fmt, good)}</div>`).join('')}</div>
  </div>`;
}

// ------------------------------------------------------------------ Telegram

async function renderTelegram(main) {
  if (!S.messages || !S.tgUsers) {
    main.innerHTML = '<div class="page"><div class="empty">Загрузка сообщений…</div></div>';
    const ok = await run(async () => {
      [S.messages, S.tgUsers] = await Promise.all([api('GET', `/messages?ym=${S.ym}`), api('GET', '/tg/users')]);
      return true;
    });
    if (!ok || S.tab !== 'telegram') return;
  }
  const bot = S.meta.bot || {};
  const unlinked = S.tgUsers.filter((u) => !u.employee_id);
  const strangers = [...new Set(S.messages.filter((m) => !m.author_id).map((m) => m.sender_name))];
  const f = S.msgFilter;
  const q = f.q.trim().toLowerCase();
  const list = S.messages.filter((m) =>
    (!f.employee || String(m.author_id) === f.employee || m.targets.some((t) => String(t.employee_id) === f.employee)) &&
    (!q || m.text.toLowerCase().includes(q) || m.sender_name.toLowerCase().includes(q)) &&
    (f.only !== 'unrecognized' || !m.recognized) && (f.only !== 'unassigned' || !m.author_id) && (f.only !== 'ignored' || m.ignored));
  const chats = Object.entries(bot.chats || {});
  main.innerHTML = `<div class="page">
    <div class="cards">
      <div class="card">
        <header><h3>Бот</h3><span class="chip ${bot.enabled ? (bot.last_error ? 'err' : '') : 'gray'}">${bot.enabled ? (bot.last_error ? 'нет связи' : 'на связи') : 'выключен'}</span></header>
        <div class="body">
          ${bot.enabled
            ? `<p>${bot.username ? `Бот <b>@${esc(bot.username)}</b>. ` : ''}${bot.last_ok ? `Последний опрос: ${esc(bot.last_ok)}.` : ''} ${bot.last_error ? `<span class="error">${esc(bot.last_error)}</span>` : ''}</p>
               <p>${chats.length ? `Группы: ${chats.map(([id, title]) => `<span class="chip gray">${esc(title || 'без названия')} · <span class="mono">${esc(id)}</span></span>`).join(' ')}` : '<span class="muted">Сообщений из групп пока не было.</span>'}</p>
               ${(S.meta.chat_ids || []).length ? `<p class="hint">Принимаются только группы: ${S.meta.chat_ids.map((x) => `<code>${esc(x)}</code>`).join(', ')}.</p>` : '<p class="hint">Принимаются сообщения из любой группы, куда добавлен бот. Чтобы ограничить, задайте <code>TELEGRAM_CHAT_ID</code>.</p>'}`
            : `<ol class="steps hint">
                <li>Создайте бота у <b>@BotFather</b> и отключите ему режим приватности: <code>/setprivacy</code> → Disable.</li>
                <li>Запишите токен в <code>TELEGRAM_BOT_TOKEN</code> (файл <code>.env</code>) и перезапустите программу.</li>
                <li>Добавьте бота в рабочую группу. Сотрудников он узнаёт по Telegram ID — привязка здесь же, ниже.</li>
              </ol>`}
        </div>
      </div>
      <div class="card">
        <header><h3>Проверить фразу</h3></header>
        <div class="body">
          <div class="row"><input id="probe-text" placeholder="Например: 17.09 уход 18:00" style="flex:1"><button class="btn" id="probe-btn">Разобрать</button></div>
          <div id="probe-out" class="hint" style="margin-top:10px">Покажет, как бот поймёт сообщение: вид отметки, время, дату и код причины.</div>
        </div>
      </div>
      <div class="card">
        <header><h3>Привязка сотрудников</h3>${unlinked.length || strangers.length ? `<span class="badge">${unlinked.length + strangers.length}</span>` : '<span class="badge ok">✓</span>'}</header>
        <div class="body">
          ${unlinked.length ? unlinked.map((u) => `<div class="row" style="margin-bottom:8px;align-items:center">
              <span style="flex:1"><b>${esc(u.name || '—')}</b> ${u.username ? `<span class="muted">@${esc(u.username)}</span>` : ''}<br><span class="hint mono">ID ${u.tg_user_id}</span></span>
              <select data-link="${u.tg_user_id}"><option value="">Кто это?</option>${S.employees.map((e) => `<option value="${e.id}" ${u.suggestion === e.id ? 'selected' : ''}>${esc(e.nick)} — ${esc(e.full_name)}</option>`).join('')}</select>
              <button class="btn small primary" data-link-ok="${u.tg_user_id}">Привязать</button></div>`).join('') : ''}
          ${strangers.length ? `<p class="hint">Сообщения без сотрудника — имя отправителя не совпало с «Именем в Telegram» в справочнике: ${strangers.map((s) => `<span class="chip warn">${esc(s)}</span>`).join(' ')}</p>` : ''}
          ${!unlinked.length && !strangers.length ? '<p class="muted">Все отправители сопоставлены с сотрудниками.</p>' : ''}
          <p class="hint">Привязанные: ${S.employees.filter((e) => e.tg_user_id).map((e) => `<span class="chip gray">${esc(e.nick)} · ${e.tg_user_id}</span>`).join(' ') || 'пока никого'}. ID также можно вписать вручную в «Справочниках»; сотрудник узнаёт свой ID командой <code>/id</code>.</p>
        </div>
      </div>
    </div>
    <div class="card">
      <header><h3>Сообщения · ${esc(S.view.title)}</h3>
        <select id="msg-emp"><option value="">Все сотрудники</option>${S.employees.map((e) => `<option value="${e.id}" ${String(e.id) === f.employee ? 'selected' : ''}>${esc(e.nick)}</option>`).join('')}</select>
        <select id="msg-only">
          <option value="">Все сообщения</option><option value="unrecognized" ${f.only === 'unrecognized' ? 'selected' : ''}>Не распознанные</option>
          <option value="unassigned" ${f.only === 'unassigned' ? 'selected' : ''}>Без сотрудника</option><option value="ignored" ${f.only === 'ignored' ? 'selected' : ''}>Не учитываются</option>
        </select>
        <input id="msg-q" placeholder="Поиск по тексту" value="${esc(f.q)}" style="width:200px">
        <button class="btn" id="paste-btn">Вставить чат</button>
        <button class="btn" id="add-msg">+ Сообщение</button>
      </header>
      <div class="body flush" style="max-height:62vh">
        ${list.length ? `<table class="tbl"><thead><tr><th>Дата</th><th>Время</th><th>Сотрудник</th><th>Сообщение</th><th>Как понято</th><th>Отнесено</th><th></th></tr></thead><tbody>
          ${list.map((m) => `<tr class="${m.ignored ? 'sub' : ''}">
            <td class="nowrap">${dm(m.sent_at)}</td><td class="nowrap"><b>${m.sent_at.slice(11, 16)}</b></td>
            <td class="nowrap">${m.author_id ? `<b>${esc(nickOf(m.author_id))}</b>` : `<span class="chip warn" title="Нет в справочнике">${esc(m.sender_name || '?')}</span>`}</td>
            <td style="${m.ignored ? 'text-decoration:line-through' : ''}">${esc(m.text)}${m.edited ? ' <span class="hint">(изменено)</span>' : ''}</td>
            <td class="nowrap"><span class="chip ${m.recognized ? (m.kind === 'info' ? 'gray' : '') : 'warn'}">${esc(m.summary)}</span></td>
            <td class="hint">${m.targets.filter((t) => t.employee_id !== m.author_id || t.date !== m.sent_at.slice(0, 10)).map((t) => `${esc(nickOf(t.employee_id))}${t.date !== m.sent_at.slice(0, 10) ? ' ' + dm(t.date) : ''}`).join(', ')}</td>
            <td class="actions"><button class="btn small ghost" data-ignore="${m.id}" data-value="${m.ignored ? 0 : 1}">${m.ignored ? 'Учитывать' : 'Не учитывать'}</button>${m.source !== 'telegram' ? `<button class="btn small ghost danger" data-del-msg="${m.id}" title="Удалить">×</button>` : ''}</td></tr>`).join('')}
          </tbody></table>` : '<div class="empty"><b>Сообщений нет</b>За этот месяц ничего не найдено. Бот добавит их сам, а прошлую переписку можно вставить кнопкой «Вставить чат».</div>'}
      </div>
    </div></div>`;
}

function pasteDialog() {
  const body = openModal('Вставить скопированный чат', `
    <p class="hint">Выделите сообщения в Telegram Desktop, скопируйте и вставьте сюда — как раньше на лист «Telegram». Уже загруженные сообщения повторно не добавятся.</p>
    <textarea name="text" rows="12" placeholder="[02.10.2026 8:44] Имя Фамилия: Офис"></textarea>
    <div class="actions"><button class="btn" data-close>Отмена</button><button class="btn primary" data-ok>Загрузить</button></div>`, true);
  $('[data-ok]', body).onclick = async () => {
    const res = await run(() => api('POST', '/messages/import', { text: $('textarea', body).value }));
    if (res) {
      closeModal();
      toast(`Добавлено сообщений: ${res.added}${res.skipped ? `, пропущено: ${res.skipped}` : ''}`);
      await reloadNow();
    }
  };
}

async function probe() {
  const text = $('#probe-text').value.trim();
  if (!text) return;
  const r = await run(() => api('POST', '/parse', { text }));
  if (!r) return;
  const parts = [`<span class="chip ${r.kind === 'unknown' ? 'warn' : ''}">${esc(r.summary)}</span>`];
  if (r.time) parts.push(`время из текста: <b>${esc(r.time)}</b>`);
  if (r.dates.length) parts.push(`дата: <b>${r.dates.map(dmy).join(', ')}</b>`);
  if (r.code) parts.push(`код: <b class="${codeClass(r.code).trim()}">${esc(r.code)}</b> (${esc(codeTitle(r.code))}${r.code_guessed ? ', по умолчанию — попадёт на проверку' : ''})`);
  if (r.mentions.length) parts.push(`отнесено к: <b>${r.mentions.map(esc).join(', ')}</b>${r.include_author ? ' и автору' : ''}`);
  if (r.kind === 'unknown') parts.push('бот не поймёт такое сообщение — оно попадёт в список «На проверку»');
  $('#probe-out').innerHTML = parts.join(' · ');
}

// ------------------------------------------------------------------ отсутствия

async function renderPeriods(main) {
  const year = Number(S.ym.slice(0, 4));
  if (!S.periods) {
    main.innerHTML = '<div class="page"><div class="empty">Загрузка…</div></div>';
    const ok = await run(async () => { S.periods = await api('GET', `/periods?year=${year}`); return true; });
    if (!ok || S.tab !== 'periods') return;
  }
  const days = (p) => Math.round((new Date(p.date_to) - new Date(p.date_from)) / 864e5) + 1;
  main.innerHTML = `<div class="page narrow">
    <div class="card">
      <header><h3>Отпуска, больничные, командировки · ${year}</h3><button class="btn primary" id="period-btn">+ Добавить период</button></header>
      <div class="body flush">${S.periods.length ? `<table class="tbl">
        <thead><tr><th>Сотрудник</th><th>Причина</th><th>С</th><th>По</th><th class="num">Дней</th><th>Примечание</th><th></th></tr></thead>
        <tbody>${S.periods.map((p) => `<tr><td><b>${esc(nickOf(p.employee_id))}</b> <span class="muted">${esc((employeeById(p.employee_id) || {}).full_name || '')}</span></td>
          <td><b class="${codeClass(p.code).trim()}">${esc(p.code)}</b> ${esc(codeTitle(p.code))}${p.tariff ? ' <span class="chip gray">тариф К1–К3</span>' : ''}</td>
          <td>${dmy(p.date_from)}</td><td>${dmy(p.date_to)}</td><td class="num">${days(p)}</td><td class="muted">${esc(p.note)}</td>
          <td class="actions"><button class="btn small ghost danger" data-del-period="${p.id}">Удалить</button></td></tr>`).join('')}</tbody></table>`
        : '<div class="empty"><b>Периодов нет</b>Добавьте отпуск, больничный или командировку — код проставится в тайм-листе на все рабочие дни периода.</div>'}</div>
    </div>
    <div class="card"><div class="body rules">
      <p><b>Как это работает.</b> Период — это то, что раньше вручную проставляли буквой в каждой строке: О, У, Б, К, Ф, Г или П. Теперь достаточно указать даты: код появится во всех рабочих днях, расчётное время посчитается по коэффициенту причины.</p>
      <p class="muted">Иногородняя командировка с тарифом расставляет К1 в дни дороги, К2 в рабочие дни и К3 в выходные. Любой день потом можно поправить в тайм-листе вручную — ручная правка всегда важнее.</p>
    </div></div></div>`;
}

// ------------------------------------------------------------------ справочники

async function renderRefs(main) {
  const year = Number(S.ym.slice(0, 4));
  if (!S.calendar || S.calendar.year !== year) {
    main.innerHTML = '<div class="page"><div class="empty">Загрузка…</div></div>';
    const ok = await run(async () => { S.calendar = await api('GET', `/calendar/${year}`); return true; });
    if (!ok || S.tab !== 'refs') return;
  }
  const s = S.settings;
  const cal = S.calendar;
  const kindName = { holiday: 'Выходной / праздник', workday: 'Рабочий день (перенос)', short: 'Сокращённый (−1 ч)' };
  main.innerHTML = `<div class="page">
    <div class="card">
      <header><h3>Сотрудники</h3><span class="hint">«Имя в Telegram» нужно для вставленного чата, ID — для бота. Псевдонимы — как человека называют в сообщениях («я и Дима на объекте»).</span><button class="btn primary" id="emp-add">+ Сотрудник</button></header>
      <div class="body flush"><table class="tbl" id="emp-table">
        <thead><tr><th style="width:52px">№</th><th>Ф.И.О.</th><th style="width:90px">Ник</th><th>Имя в Telegram</th><th style="width:130px">Telegram ID</th><th>Псевдонимы</th><th class="c" title="Сотрудник не отмечается в группе: в рабочие дни автоматически ставится полный день">Без отметок</th><th class="c">Работает</th><th></th></tr></thead>
        <tbody>${S.employees.map(employeeRow).join('') || '<tr><td colspan="9" class="empty">Список пуст. Добавьте сотрудников или импортируйте книгу Excel ниже.</td></tr>'}</tbody>
      </table></div>
    </div>
    <div class="cards">
      <div class="card">
        <header><h3>Причины и коэффициенты</h3></header>
        <div class="body flush"><table class="tbl">
          <thead><tr><th>Код</th><th>Причина</th><th style="width:96px">Коэфф.</th></tr></thead>
          <tbody>${S.codes.map((c) => `<tr><td><b class="${codeClass(c.code).trim()}">${esc(c.code)}</b></td><td><b>${esc(c.title)}</b><br><span class="hint">${esc(c.description)}</span></td>
            <td><input type="number" step="0.05" min="0" max="10" value="${c.coef}" data-code="${esc(c.code)}"></td></tr>`).join('')}</tbody>
        </table></div>
      </div>
      <div class="card">
        <header><h3>Рабочее время</h3></header>
        <div class="body">
          <div class="form-grid" id="settings-form">
            <label class="field">Начало дня<input name="work_start" value="${esc(s.work_start)}"></label>
            <label class="field">Конец дня<input name="work_end" value="${esc(s.work_end)}"></label>
            <label class="field">Обед<input name="lunch" value="${esc(s.lunch)}"></label>
            <label class="field" title="Используется для новых месяцев; у каждого месяца лимит можно изменить в «Сводке»">Лимит раб. времени, %<input name="limit" type="number" min="1" value="${Math.round(s.limit * 1000) / 10}"></label>
            <label class="field" title="Смещение местного времени от UTC">Часовой пояс, UTC+<input name="utc" type="number" step="0.5" value="${s.utc_offset_minutes / 60}"></label>
            <label class="check full"><input type="checkbox" name="trip_use_times" ${s.trip_use_times ? 'checked' : ''}> В командировке с тарифом К1–К3 считать время из сообщений (иначе 8 ч × тариф)</label>
            <div class="full actions" style="display:flex;justify-content:flex-end"><button class="btn primary" id="settings-save">Сохранить</button></div>
          </div>
        </div>
      </div>
      <div class="card">
        <header><h3>Excel</h3></header>
        <div class="body">
          <p>Импорт прежней книги <code>TimeList-ГГГГ-М.xlsm</code>: сотрудники, коэффициенты, нормы, отметки месяца и сообщения с листа «Telegram».</p>
          <div class="row"><input type="file" id="xlsx-file" accept=".xlsm,.xlsx" style="flex:1"><button class="btn primary" id="xlsx-import">Импортировать</button></div>
          <p class="hint" style="margin-top:8px">Отметки импортируются как ручные, поэтому цифры месяца совпадут с книгой.</p>
          <p style="margin-top:14px"><a class="btn" href="/api/export/${S.ym}" download>Выгрузить ${esc(S.view ? S.view.title : S.ym)} в Excel</a></p>
        </div>
      </div>
    </div>
    <div class="cards">
      <div class="card">
        <header><h3>Нормы рабочего времени · ${year}</h3><span class="hint">5-дневная неделя</span></header>
        <div class="body flush"><table class="tbl">
          <thead><tr><th>Месяц</th><th style="width:110px">Часов</th><th style="width:96px">Дней</th><th>Источник</th><th></th></tr></thead>
          <tbody>${cal.months.map((m) => `<tr><td>${esc(m.name)}</td><td><input type="number" step="1" min="1" value="${m.hours}" data-norm-h="${m.month}"></td><td><input type="number" min="1" max="31" value="${m.days}" data-norm-d="${m.month}"></td>
            <td class="hint">${m.source === 'table' ? `задана${m.hours !== m.calc_hours || m.days !== m.calc_days ? ` (по календарю ${m.calc_hours} ч / ${m.calc_days} дн.)` : ''}` : 'по календарю'}</td>
            <td class="actions">${m.source === 'table' ? `<button class="btn small ghost" data-norm-reset="${m.month}" title="Считать по календарю">↺</button>` : ''}</td></tr>`).join('')}</tbody>
        </table></div>
      </div>
      <div class="card">
        <header><h3>Праздники и переносы · ${year}</h3></header>
        <div class="body">
          <div class="row" id="day-form">
            <label class="field">Дата<input type="date" name="date" value="${year}-01-01"></label>
            <label class="field">Тип<select name="kind">${Object.entries(kindName).map(([k, n]) => `<option value="${k}">${n}</option>`).join('')}</select></label>
            <label class="field" style="flex:1">Название<input name="title" placeholder="Например: Навруз"></label>
            <button class="btn primary" id="day-add">Добавить</button>
          </div>
        </div>
        <div class="body flush" style="max-height:420px"><table class="tbl">
          <tbody>${cal.days.map((d) => `<tr><td class="nowrap"><b>${dmy(d.date)}</b></td><td><span class="chip ${d.kind === 'holiday' ? 'err' : d.kind === 'short' ? 'warn' : ''}">${kindName[d.kind]}</span></td><td>${esc(d.title)}</td>
            <td class="actions"><button class="btn small ghost danger" data-day-del="${d.date}">×</button></td></tr>`).join('') || '<tr><td class="empty">На этот год особых дней нет: рабочими считаются Пн–Пт.</td></tr>'}</tbody>
        </table></div>
      </div>
      <div class="card">
        <header><h3>Памятка</h3></header>
        <div class="body rules">
          <p><b>Что писать в группу Telegram</b></p>
          <ul>
            <li>в начале дня — <code>офис</code>; если до офиса ездили по делам: <code>офис, до банк</code>;</li>
            <li>при выезде больше чем на час — <code>выезд на объект …</code>, по возвращении — <code>вернулся</code>;</li>
            <li>в конце дня — <code>уход</code> или <code>убыл</code>;</li>
            <li>опоздание или отсутствие — с причиной: <code>задержусь у врача</code>, <code>болею</code>, <code>отгул</code>;</li>
            <li>забыли отметиться — время в тексте: <code>офис 9:10</code>, за прошлый день: <code>17.09 уход 18:00</code>.</li>
          </ul>
          <p><b>Как считается</b></p>
          <ul>
            <li>Отработано = Конец − Начало − Уходы − обед.</li>
            <li>Код причины возвращает в расчётное время долю опоздания, отлучки или раннего ухода: коэффициент 1 — всё, 0,8 — 80 %, 0 — ничего.</li>
            <li>Код без времени — день отсутствия: 8 ч × коэффициент. Тариф К1–К3 умножает время дня в командировке.</li>
            <li>КТУ = учтённое время / норма месяца, не больше лимита.</li>
          </ul>
        </div>
      </div>
    </div></div>`;
}

function employeeRow(e) {
  return `<tr data-id="${e.id || ''}">
    <td><input type="number" name="sort" value="${e.sort ?? ''}"></td>
    <td><input name="full_name" value="${esc(e.full_name || '')}" placeholder="Фамилия И.О."></td>
    <td><input name="nick" value="${esc(e.nick || '')}" placeholder="ИвИ"></td>
    <td><input name="tg_name" value="${esc(e.tg_name || '')}" placeholder="Имя Фамилия"></td>
    <td><input name="tg_user_id" value="${e.tg_user_id ?? ''}" inputmode="numeric" placeholder="—"></td>
    <td><input name="aliases" value="${esc(e.aliases || '')}" placeholder="Дима, Дмитрий"></td>
    <td class="c"><input type="checkbox" name="auto_schedule" ${e.auto_schedule ? 'checked' : ''}></td>
    <td class="c"><input type="checkbox" name="active" ${e.active === false ? '' : 'checked'}></td>
    <td class="actions"><button class="btn small primary" data-emp-save>Сохранить</button>${e.id ? '<button class="btn small ghost danger" data-emp-del title="Удалить вместе с отметками">×</button>' : ''}</td></tr>`;
}

async function saveEmployee(tr) {
  const v = (n) => $(`[name="${n}"]`, tr);
  const id = tr.dataset.id;
  const old = id ? employeeById(Number(id)) : {};
  const tg = v('tg_user_id').value.trim();
  if (tg && !/^-?\d+$/.test(tg)) return toast('Telegram ID — это число. Сотрудник узнаёт его командой /id', true);
  const payload = {
    ...old,
    full_name: v('full_name').value, nick: v('nick').value, tg_name: v('tg_name').value, tg_user_id: tg ? Number(tg) : null,
    aliases: v('aliases').value, auto_schedule: v('auto_schedule').checked, active: v('active').checked, sort: Number(v('sort').value) || 0,
  };
  const ok = await run(() => (id ? api('PUT', `/employees/${id}`, payload) : api('POST', '/employees', payload)));
  if (ok) {
    toast('Сохранено');
    await run(async () => {
      await Promise.all([loadRefs(), loadMonth()]);
      render(true);
    });
  }
}

// ------------------------------------------------------------------ события

function bind() {
  $('#tabs').addEventListener('click', (ev) => {
    const b = ev.target.closest('button[data-tab]');
    if (!b) return;
    S.tab = b.dataset.tab;
    setHash();
    render();
  });
  $('#month-prev').onclick = () => shiftMonth(-1);
  $('#month-next').onclick = () => shiftMonth(1);
  $('#month-today').onclick = () => goMonth(S.meta.today.slice(0, 7));
  $('#month-input').onchange = (ev) => goMonth(ev.target.value);
  $('#theme-btn').onclick = () => {
    const next = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    localStorage.setItem('tl_theme', next);
  };
  $('#user-btn').onclick = () => {
    const name = prompt('Ваше имя — оно попадёт в журнал изменений:', S.user);
    if (name == null) return;
    S.user = name.trim();
    localStorage.setItem('tl_user', S.user);
    render(true);
  };
  $('#logout-btn').onclick = async () => {
    await run(() => api('POST', '/logout'));
    location.reload();
  };
  $('#modal').addEventListener('click', (ev) => {
    if (ev.target.id === 'modal' || ev.target.closest('#modal-close, [data-close]')) closeModal();
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape' && !$('#modal').hidden) closeModal();
  });
  $('#login-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    try {
      await api('POST', '/login', { password: $('#login-password').value });
      location.reload();
    } catch (e) {
      $('#login-error').textContent = e.message;
      $('#login-error').hidden = false;
    }
  });

  const main = $('#main');
  main.addEventListener('keydown', (ev) => {
    if (ev.target.closest('#grid')) onGridKey(ev);
    else if (ev.key === 'Enter' && ev.target.closest('#side') && ev.target.tagName === 'INPUT') ev.target.blur();
    else if (ev.key === 'Enter' && ev.target.id === 'probe-text') probe();
  });
  main.addEventListener('dblclick', (ev) => {
    if (ev.target.closest('#grid td[data-e]')) startEdit(null);
  });
  main.addEventListener('change', async (ev) => {
    const t = ev.target;
    if (t.closest('#side')) return onSideChange(ev);
    if (t.id === 'msg-emp') { S.msgFilter.employee = t.value; render(true); }
    else if (t.id === 'msg-only') { S.msgFilter.only = t.value; render(true); }
    else if (t.id === 'limit-input') {
      const limit = Number(t.value) / 100;
      await run(async () => { S.view = await api('PUT', `/months/${S.ym}/limit`, { limit }); render(true); toast('Лимит сохранён'); });
    } else if (t.dataset.code) {
      const code = S.codes.find((c) => c.code === t.dataset.code);
      const ok = await run(() => api('PUT', `/codes/${encodeURIComponent(code.code)}`, { coef: Number(t.value) }));
      if (ok) { toast(`Коэффициент ${code.code} сохранён`); await loadRefs(); await loadMonth(); }
    } else if (t.dataset.normH || t.dataset.normD) {
      const month = t.dataset.normH || t.dataset.normD;
      const hours = Number($(`[data-norm-h="${month}"]`).value);
      const days = Number($(`[data-norm-d="${month}"]`).value);
      const ok = await run(() => api('PUT', `/calendar/${S.calendar.year}/norms/${month}`, { hours, days }));
      if (ok) { toast('Норма сохранена'); S.calendar = null; await loadMonth(); render(true); }
    }
  });
  main.addEventListener('input', debounce((ev) => {
    if (ev.target.id !== 'msg-q') return;
    S.msgFilter.q = ev.target.value;
    const pos = ev.target.selectionStart;
    render(true);
    const again = $('#msg-q');
    again.focus();
    again.setSelectionRange(pos, pos);
  }, 250));

  main.addEventListener('click', async (ev) => {
    const t = ev.target;
    const cell = t.closest('#grid td[data-e]');
    if (cell) return select(Number(cell.dataset.e), Number(cell.closest('tr').dataset.day), Number(cell.dataset.f));
    const empHead = t.closest('#grid th.emp');
    if (empHead) { S.tabelEmp = S.view.employees[Number(empHead.dataset.emp)].id; S.tab = 'tabel'; setHash(); return render(); }
    if (t.closest('#side')) return onSideClick(ev);
    const go = t.closest('[data-go]');
    if (go) { S.tab = go.dataset.go; setHash(); return render(); }
    const seg = t.closest('#view-seg button');
    if (seg) { S.compact = seg.dataset.compact === '1'; localStorage.setItem('tl_compact', S.compact ? '1' : '0'); return render(); }
    if (t.closest('#issues-btn')) return issuesDialog();
    if (t.closest('#period-btn')) return periodDialog();
    if (t.closest('#lock-btn')) return toggleLock();
    if (t.closest('#side-btn')) return toggleSide();
    const te = t.closest('#tabel-emp button');
    if (te) { S.tabelEmp = Number(te.dataset.id); return render(true); }

    // Telegram
    if (t.closest('#probe-btn')) return probe();
    if (t.closest('#paste-btn')) return pasteDialog();
    if (t.closest('#add-msg')) return addMessageDialog(null, S.view.today.slice(0, 7) === S.ym ? S.view.today : `${S.ym}-01`);
    const ign = t.closest('[data-ignore]');
    if (ign) { await run(() => api('PATCH', `/messages/${ign.dataset.ignore}`, { ignored: ign.dataset.value === '1' })); return reloadNow(); }
    const delMsg = t.closest('[data-del-msg]');
    if (delMsg && confirm('Удалить сообщение?')) { await run(() => api('DELETE', `/messages/${delMsg.dataset.delMsg}`)); return reloadNow(); }
    const link = t.closest('[data-link-ok]');
    if (link) {
      const employee = $(`select[data-link="${link.dataset.linkOk}"]`).value;
      if (!employee) return toast('Выберите сотрудника', true);
      const ok = await run(() => api('POST', `/tg/users/${link.dataset.linkOk}/link`, { employee_id: Number(employee) }));
      if (ok) { toast('Привязано'); S.tgUsers = null; await loadRefs(); return reloadNow(); }
    }

    // периоды
    const delPeriod = t.closest('[data-del-period]');
    if (delPeriod && confirm('Удалить период? Код исчезнет из дней, где его не правили вручную.')) {
      await run(() => api('DELETE', `/periods/${delPeriod.dataset.delPeriod}`));
      S.periods = null;
      return reloadNow();
    }

    // справочники
    if (t.closest('#emp-add')) {
      const body = $('#emp-table tbody');
      if (!S.employees.length) body.innerHTML = '';
      body.insertAdjacentHTML('beforeend', employeeRow({ sort: S.employees.length + 1, active: true }));
      return $('tr:last-child [name="full_name"]', body).focus();
    }
    if (t.closest('[data-emp-save]')) return saveEmployee(t.closest('tr'));
    if (t.closest('[data-emp-del]')) {
      const tr = t.closest('tr');
      const e = employeeById(Number(tr.dataset.id));
      if (!confirm(`Удалить ${e.nick} вместе со всеми отметками и периодами? Если человек просто больше не работает, снимите галочку «Работает».`)) return;
      await run(() => api('DELETE', `/employees/${e.id}`));
      await loadRefs();
      return reloadNow();
    }
    if (t.closest('#settings-save')) {
      const f = $('#settings-form');
      const v = (n) => $(`[name="${n}"]`, f);
      const payload = {
        work_start: v('work_start').value, work_end: v('work_end').value, lunch: v('lunch').value, limit: Number(v('limit').value) / 100,
        utc_offset_minutes: Math.round(Number(v('utc').value) * 60), trip_use_times: v('trip_use_times').checked,
      };
      const ok = await run(() => api('PUT', '/settings', payload));
      if (ok) { toast('Настройки сохранены'); await loadRefs(); return reloadNow(); }
    }
    const normReset = t.closest('[data-norm-reset]');
    if (normReset) { await run(() => api('DELETE', `/calendar/${S.calendar.year}/norms/${normReset.dataset.normReset}`)); S.calendar = null; return reloadNow(); }
    if (t.closest('#day-add')) {
      const f = $('#day-form');
      const date = $('[name="date"]', f).value;
      const ok = await run(() => api('PUT', `/calendar/days/${date}`, { kind: $('[name="kind"]', f).value, title: $('[name="title"]', f).value }));
      if (ok) { S.calendar = null; return reloadNow(); }
    }
    const dayDel = t.closest('[data-day-del]');
    if (dayDel) { await run(() => api('DELETE', `/calendar/days/${dayDel.dataset.dayDel}`)); S.calendar = null; return reloadNow(); }
    if (t.closest('#xlsx-import')) {
      const file = $('#xlsx-file').files[0];
      if (!file) return toast('Выберите файл книги', true);
      const r = await run(async () => api('POST', '/import/xlsx', await file.arrayBuffer(), true));
      if (r) {
        toast(`Импортирован ${r.month}: сотрудников ${r.employees_total}, дней с отметками ${r.mark_days}, сообщений ${r.messages_added}`);
        await loadRefs();
        if (r.month) { S.tab = 'timelist'; return goMonth(r.month); }
      }
    }
  });
}

function connectEvents() {
  const es = new EventSource('/api/events');
  es.addEventListener('change', () => scheduleReload());
  es.onerror = () => { /* браузер переподключится сам */ };
}

async function boot() {
  bind();
  await loadMeta();
  if (S.meta.auth_required && !S.meta.authed) return showLogin();
  $('#logout-btn').hidden = !S.meta.auth_required;
  const [tab, ym] = location.hash.slice(1).split('/');
  if (['timelist', 'tabel', 'svodka', 'telegram', 'periods', 'refs'].includes(tab)) S.tab = tab;
  S.ym = /^\d{4}-\d{2}$/.test(ym || '') ? ym : S.meta.today.slice(0, 7);
  await run(async () => {
    await Promise.all([loadRefs(), loadMonth()]);
    setHash();
    render();
    if (S.tab === 'timelist' && $('#grid')) $('#grid').focus({ preventScroll: true });
  });
  connectEvents();
  setInterval(() => run(loadMeta), 60000);
}

$('#login-form') && boot();
