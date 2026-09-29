// Aegis dashboard: a small single-page app with no build step and no
// dependencies. It talks to the same /api/v1 endpoints as the CLI, signed in
// with the session cookie. The page's content security policy forbids inline
// scripts and styles, so everything here builds DOM nodes directly (never
// innerHTML with data) and sets styles only through the CSSOM.

'use strict';

// ----------------------------------------------------------------------
// DOM helpers
// ----------------------------------------------------------------------

/** h('div', {class: 'x', onclick: fn}, 'text', child, [more]) */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on') && typeof value === 'function') {
      el.addEventListener(key.slice(2), value);
    } else if (key === 'dataset') {
      Object.assign(el.dataset, value);
    } else if (['value', 'checked', 'disabled', 'selected', 'open'].includes(key)) {
      el[key] = value;
    } else if (key === 'pct') {
      el.style.setProperty('--pct', `${Math.max(0, Math.min(100, value))}%`);
    } else {
      el.setAttribute(key, value === true ? '' : String(value));
    }
  }
  append(el, children);
  return el;
}

function append(el, ...children) {
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

function replace(el, ...children) {
  el.replaceChildren();
  return append(el, children);
}

const $ = (sel, root = document) => root.querySelector(sel);

// ----------------------------------------------------------------------
// Formatting
// ----------------------------------------------------------------------

function fmtBytes(n) {
  if (n === null || n === undefined) return '–';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 || i === 0 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

function fmtDuration(ms) {
  if (ms === null || ms === undefined || Number.isNaN(ms)) return '–';
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const hr = Math.floor(m / 60);
  if (hr < 48) return `${hr}h ${m % 60}m`;
  return `${Math.floor(hr / 24)}d ${hr % 24}h`;
}

function parseTime(iso) {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? null : t;
}

function fmtAgo(iso) {
  const t = parseTime(iso);
  if (t === null) return '–';
  const diff = Date.now() - t;
  if (diff < 45_000) return 'just now';
  return `${fmtDuration(diff).split(' ')[0]} ago`;
}

function fmtTime(iso) {
  const t = parseTime(iso);
  if (t === null) return '–';
  const d = new Date(t);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay
    ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
    : d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

function timeEl(iso, text) {
  return h('time', { datetime: iso, title: iso ? new Date(iso).toLocaleString() : '' }, text ?? fmtTime(iso));
}

function shortId(id) {
  return id ? id.slice(-8) : '';
}

function durationBetween(start, end) {
  const a = parseTime(start);
  const b = end ? parseTime(end) : Date.now();
  return a === null || b === null ? null : b - a;
}

// ----------------------------------------------------------------------
// API client
// ----------------------------------------------------------------------

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

const state = {
  session: null,
  csrf: null,
  projects: [],
  cleanups: [],
  routeToken: 0,
  events: null,
  eventListeners: new Set(),
  shell: null,
};

async function api(method, path, body) {
  const opts = { method, credentials: 'same-origin', headers: { Accept: 'application/json' } };
  if (method !== 'GET') {
    // Every state-changing request must be JSON (see the server's request guard).
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body ?? {});
    if (state.csrf) opts.headers['X-CSRF-Token'] = state.csrf;
  }
  let res;
  try {
    res = await fetch(`/api/v1${path}`, opts);
  } catch {
    throw new ApiError(0, 'network', 'Could not reach the Aegis daemon');
  }
  const text = await res.text();
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (!res.ok) {
    const message = data?.error?.message || `${res.status} ${res.statusText}`;
    if (res.status === 401 && path !== '/auth/login' && path !== '/auth/password') signedOut();
    throw new ApiError(res.status, data?.error?.code, message);
  }
  return data;
}

const enc = encodeURIComponent;

// ----------------------------------------------------------------------
// Toasts, dialogs, busy buttons
// ----------------------------------------------------------------------

function toast(message, kind = '') {
  const el = h('div', { class: `toast ${kind}` }, message);
  $('#toasts').append(el);
  setTimeout(() => el.remove(), kind === 'bad' ? 7000 : 3500);
}

function modal(build) {
  return new Promise((resolve) => {
    const dlg = h('dialog', {});
    dlg.addEventListener('close', () => {
      resolve(dlg.returnValue);
      dlg.remove();
    });
    append(dlg, [build(dlg)]);
    document.body.append(dlg);
    dlg.showModal();
  });
}

/** Resolves to true when the operator confirms. */
async function confirmDialog({ title, message, confirmLabel = 'Confirm', danger = false }) {
  const result = await modal(() =>
    h('form', { method: 'dialog' },
      h('div', { class: 'dialog-body' }, h('h2', {}, title), h('p', { class: 'muted' }, message)),
      h('div', { class: 'form-actions' },
        h('button', { class: 'btn', value: 'cancel', type: 'submit' }, 'Cancel'),
        h('button', {
          class: `btn ${danger ? 'danger solid' : 'primary'}`,
          value: 'ok',
          type: 'submit',
          autofocus: true,
          'data-confirm': '',
        }, confirmLabel))));
  return result === 'ok';
}

/** Runs `fn` with `btn` disabled; errors become a toast. */
async function run(btn, fn, success) {
  if (btn) btn.disabled = true;
  try {
    const result = await fn();
    if (success) toast(typeof success === 'function' ? success(result) : success);
    return result;
  } catch (err) {
    if (err.status !== 401) toast(err.message, 'bad');
    return undefined;
  } finally {
    if (btn) btn.disabled = false;
  }
}

function errorBox(err) {
  return h('div', { class: 'alert', role: 'alert' }, err.message || String(err));
}

function copyButton(text) {
  return h('button', {
    class: 'btn small',
    type: 'button',
    onclick: async () => {
      try {
        await navigator.clipboard.writeText(text);
        toast('Copied');
      } catch {
        toast('Copy failed; select the text instead', 'bad');
      }
    },
  }, 'Copy');
}

function secretBox(text, label) {
  return h('div', { class: 'secret', 'aria-label': label || 'Secret' }, h('code', { 'data-secret': '' }, text), copyButton(text));
}

// ----------------------------------------------------------------------
// Theme
// ----------------------------------------------------------------------

function storedTheme() {
  try {
    return localStorage.getItem('aegis.theme') || 'system';
  } catch {
    return 'system';
  }
}

function applyTheme(pref) {
  if (pref === 'light' || pref === 'dark') document.documentElement.dataset.theme = pref;
  else delete document.documentElement.dataset.theme;
  try {
    localStorage.setItem('aegis.theme', pref);
  } catch {
    /* private mode: the choice lasts for this page only */
  }
}

function effectiveTheme() {
  const pref = document.documentElement.dataset.theme;
  if (pref) return pref;
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function toggleTheme() {
  applyTheme(effectiveTheme() === 'dark' ? 'light' : 'dark');
}

applyTheme(storedTheme());

// ----------------------------------------------------------------------
// Status vocabulary
// ----------------------------------------------------------------------

function processPill(project) {
  const p = project.process;
  if (!p) {
    return project.current_release ? pill('Stopped', '') : pill('Not deployed', '');
  }
  switch (p.status) {
    case 'Running': return pill('Running', 'ok');
    case 'Backoff': return pill('Restarting', 'warn live');
    case 'Stopping': return pill('Stopping', 'warn live');
    case 'Failed': return pill('Failed', 'bad');
    case 'Exited': return pill('Exited', 'warn');
    default: return pill(p.status, '');
  }
}

function deploymentPill(status) {
  switch (status) {
    case 'Queued': return pill('Queued', 'info');
    case 'InProgress': return pill('Deploying', 'info live');
    case 'Success': return pill('Success', 'ok');
    case 'Failed': return pill('Failed', 'bad');
    case 'RolledBack': return pill('Rolled back', 'warn');
    default: return pill(status, '');
  }
}

function releasePill(status) {
  switch (status) {
    case 'Active': return pill('Live', 'ok');
    case 'Failed': return pill('Failed', 'bad');
    default: return pill(status, '');
  }
}

function pill(text, kind) {
  return h('span', { class: `pill ${kind}`, 'data-status': text }, text);
}

const isActiveDeployment = (d) => d.status === 'Queued' || d.status === 'InProgress';

function uptime(process) {
  if (!process || process.status !== 'Running' || !process.last_start) return '–';
  return fmtDuration(durationBetween(process.last_start));
}

function sourceText(project) {
  if (project.repository_url) return `${project.repository_url} @ ${project.branch}`;
  if (project.source_dir) return project.source_dir;
  return 'No source configured';
}

// ----------------------------------------------------------------------
// Live events (one EventSource shared by every page)
// ----------------------------------------------------------------------

// The stream names each message after its event type, so the browser needs
// a listener per type.
const EVENT_TYPES = [
  'ProjectCreated', 'ProjectSettingsUpdated', 'ScheduleConfigured', 'ScheduledAutoDeploy',
  'DeploymentQueued', 'DeploymentStarted', 'DeploymentCompleted', 'DeploymentSucceeded',
  'DeploymentFailed', 'DeploymentRolledBack',
  'BuildStageClone', 'BuildStageInstall', 'BuildStageBuild', 'BuildStageTest',
  'BuildStagePackage', 'BuildStageVerify', 'BuildStagePromote',
  'ReleaseCreated', 'ReleasePromoted', 'ReleaseArchived',
  'RollbackRequested', 'RollbackCompleted', 'RollbackFailed',
  'ProcessStartRequested', 'ProcessStopRequested', 'ProcessRestartRequested',
  'ProcessStarted', 'ProcessCrashed', 'ProcessFailed', 'ProcessStopped',
  'UserLoggedIn', 'UserLoginFailed', 'UserLoggedOut', 'PasswordChanged', 'PasswordReset',
  'ApiTokenCreated', 'ApiTokenRevoked', 'WebhookSecretRotated',
];

function connectEvents() {
  if (state.events) return;
  const source = new EventSource('/api/v1/events/stream');
  state.events = source;
  const deliver = (msg) => {
    let event;
    try {
      event = JSON.parse(msg.data);
    } catch {
      return;
    }
    for (const fn of state.eventListeners) fn(event);
  };
  for (const type of EVENT_TYPES) source.addEventListener(type, deliver);
  // Missed events (lag or a reconnect): pages reload everything.
  const resync = () => {
    for (const fn of state.eventListeners) fn({ event_type: '_resync', payload: {} });
  };
  source.addEventListener('lagged', resync);
  let failed = false;
  source.addEventListener('open', () => {
    if (failed) resync();
    failed = false;
    setConnection(true);
  });
  source.addEventListener('error', () => {
    failed = true;
    setConnection(false);
    // The browser reconnects by itself; find out whether the session ended.
    api('GET', '/auth/session').catch(() => {});
  });
}

function disconnectEvents() {
  if (state.events) state.events.close();
  state.events = null;
}

function setConnection(ok) {
  const el = $('#conn');
  if (!el) return;
  el.className = `pill ${ok ? 'ok' : 'warn live'}`;
  el.textContent = ok ? 'Live' : 'Reconnecting';
}

/** Calls `fn` for live events while the current page is shown. */
function onEvents(ctx, fn) {
  state.eventListeners.add(fn);
  ctx.cleanup(() => state.eventListeners.delete(fn));
}

function debounce(fn, ms) {
  let timer = null;
  const wrapped = (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
  wrapped.cancel = () => clearTimeout(timer);
  return wrapped;
}

function every(ctx, ms, fn) {
  const id = setInterval(fn, ms);
  ctx.cleanup(() => clearInterval(id));
}

// ----------------------------------------------------------------------
// Router
// ----------------------------------------------------------------------

const ROUTES = [
  [/^\/?$/, overviewPage, 'overview'],
  [/^\/apps\/new$/, wizardPage, 'overview'],
  [/^\/apps\/([^/]+)(?:\/([a-z]+))?$/, appPage, 'overview'],
  [/^\/deployments$/, deploymentsPage, 'deployments'],
  [/^\/deployments\/([^/]+)$/, deploymentPage, 'deployments'],
  [/^\/events$/, eventsPage, 'events'],
  [/^\/account$/, accountPage, 'account'],
];

function currentPath() {
  return decodeURI(location.hash.replace(/^#/, '')) || '/';
}

function go(path) {
  location.hash = `#${path}`;
}

function teardownPage() {
  state.routeToken += 1;
  for (const fn of state.cleanups.splice(0)) {
    try {
      fn();
    } catch {
      /* ignore */
    }
  }
}

async function render() {
  teardownPage();
  if (!state.session) {
    loginPage();
    return;
  }
  const path = currentPath();
  let match = null;
  let page = notFoundPage;
  let section = '';
  for (const [re, fn, nav] of ROUTES) {
    match = path.match(re);
    if (match) {
      page = fn;
      section = nav;
      break;
    }
  }
  const main = shell(section);
  const token = state.routeToken;
  const ctx = {
    alive: () => token === state.routeToken,
    cleanup: (fn) => state.cleanups.push(fn),
  };
  try {
    await page(main, ctx, ...(match ? match.slice(1).map((p) => (p ? decodeURIComponent(p) : p)) : []));
  } catch (err) {
    if (ctx.alive() && err.status !== 401) replace(main, errorBox(err));
  }
}

function shell(section) {
  if (!state.shell) {
    const nav = h('nav', { class: 'nav', 'aria-label': 'Main' },
      h('a', { href: '#/', 'data-nav': 'overview' }, 'Overview'),
      h('a', { href: '#/deployments', 'data-nav': 'deployments' }, 'Deployments'),
      h('a', { href: '#/events', 'data-nav': 'events' }, 'Events'),
      h('a', { href: '#/account', 'data-nav': 'account' }, 'Account'));
    const main = h('main', { id: 'main' });
    const bar = h('header', { class: 'topbar' },
      h('a', { class: 'brand', href: '#/' }, h('img', { src: '/assets/favicon.svg', alt: '' }), h('span', {}, 'Aegis')),
      nav,
      h('div', { class: 'topbar-actions' },
        h('span', { id: 'conn', class: 'pill warn live' }, 'Connecting'),
        h('button', { class: 'btn ghost icon', type: 'button', title: 'Toggle theme (t)', 'aria-label': 'Toggle theme', onclick: toggleTheme }, '◐'),
        h('button', { class: 'btn ghost', type: 'button', id: 'signout', onclick: signOut }, 'Sign out')));
    state.shell = { root: h('div', {}, bar, main), main };
    replace($('#app'), state.shell.root);
  }
  for (const a of state.shell.root.querySelectorAll('[data-nav]')) {
    if (a.dataset.nav === section) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  replace(state.shell.main);
  window.scrollTo(0, 0);
  return state.shell.main;
}

function pageHeader(title, ...right) {
  return h('div', { class: 'page-header' }, h('h1', {}, title), h('div', { class: 'spacer' }), right);
}

function notFoundPage(main) {
  append(main, h('div', { class: 'card empty' }, h('h2', {}, 'Page not found'), h('a', { class: 'btn', href: '#/' }, 'Back to overview')));
}

// ----------------------------------------------------------------------
// Sign in / out
// ----------------------------------------------------------------------

function signedOut() {
  if (!state.session) return;
  state.session = null;
  state.csrf = null;
  disconnectEvents();
  render();
}

async function signOut() {
  try {
    await api('POST', '/auth/logout');
  } catch {
    /* the session may already be gone */
  }
  state.session = null;
  state.csrf = null;
  disconnectEvents();
  render();
}

function startSession(session) {
  state.session = session;
  state.csrf = session.csrf_token;
  state.shell = null;
  connectEvents();
  render();
}

function loginPage() {
  state.shell = null;
  const error = h('div');
  const username = h('input', { id: 'username', name: 'username', autocomplete: 'username', required: true, value: 'admin' });
  const password = h('input', { id: 'password', name: 'password', type: 'password', autocomplete: 'current-password', required: true, autofocus: true });
  const submit = h('button', { class: 'btn primary', type: 'submit' }, 'Sign in');
  const form = h('form', {
    class: 'card card-body',
    onsubmit: async (e) => {
      e.preventDefault();
      replace(error);
      submit.disabled = true;
      try {
        const session = await api('POST', '/auth/login', { username: username.value.trim(), password: password.value });
        startSession(session);
      } catch (err) {
        append(error, errorBox(err));
        password.select();
      } finally {
        submit.disabled = false;
      }
    },
  },
  error,
  h('div', { class: 'field' }, h('label', { for: 'username' }, 'Username'), username),
  h('div', { class: 'field' }, h('label', { for: 'password' }, 'Password'), password),
  submit);
  replace($('#app'), h('div', { class: 'login-wrap' },
    h('div', { class: 'login' },
      h('div', { class: 'brand' }, h('img', { src: '/assets/favicon.svg', alt: '' }), h('span', {}, 'Aegis')),
      form,
      h('p', { class: 'muted small' }, 'Forgot the password? On the server run ', h('code', {}, 'sudo aegis-daemon admin reset-password'), '.'))));
  (username.value ? password : username).focus();
}

// ----------------------------------------------------------------------
// Overview
// ----------------------------------------------------------------------

async function overviewPage(main, ctx) {
  const hostEl = h('div', { class: 'grid stats', id: 'host' });
  const appsEl = h('div', { id: 'apps' });
  append(main,
    pageHeader('Overview', h('a', { class: 'btn primary', href: '#/apps/new' }, '+ New app')),
    hostEl,
    h('div', { class: 'section' }, h('h2', {}, 'Apps'), appsEl));

  const loadHost = async () => {
    try {
      const host = await api('GET', '/host');
      if (ctx.alive()) replace(hostEl, hostTiles(host));
    } catch (err) {
      if (ctx.alive()) replace(hostEl, errorBox(err));
    }
  };
  const loadApps = async () => {
    const [projects, deployments] = await Promise.all([api('GET', '/projects'), api('GET', '/deployments')]);
    state.projects = projects;
    if (!ctx.alive()) return;
    if (projects.length === 0) {
      replace(appsEl, h('div', { class: 'card empty' },
        h('h2', {}, 'No apps yet'),
        h('p', {}, 'Point Aegis at a git repository or a directory on this server.'),
        h('a', { class: 'btn primary', href: '#/apps/new' }, 'Create your first app')));
      return;
    }
    const active = new Map();
    for (const d of deployments) if (isActiveDeployment(d)) active.set(d.project_id, d);
    replace(appsEl, h('div', { class: 'grid apps' }, projects.map((p) => appCard(p, active.get(p.id), loadApps))));
  };

  await Promise.all([loadHost(), loadApps()]);
  every(ctx, 5000, loadHost);
  // Uptimes tick even when nothing happens.
  every(ctx, 30000, () => loadApps().catch(() => {}));
  const reload = debounce(() => loadApps().catch(() => {}), 250);
  onEvents(ctx, (e) => {
    if (!/^(User|Password|ApiToken)/.test(e.event_type)) reload();
  });
  ctx.cleanup(reload.cancel);
}

function meterClass(pct) {
  if (pct >= 90) return 'meter bad';
  if (pct >= 75) return 'meter warn';
  return 'meter';
}

function tile(label, value, pct, detail, id) {
  return h('div', { class: 'card stat', 'data-tile': id },
    h('div', { class: 'label' }, label),
    h('div', { class: 'value' }, value),
    pct === null ? null : h('div', { class: meterClass(pct), role: 'meter', 'aria-valuenow': Math.round(pct), 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': label }, h('span', { pct })),
    h('div', { class: 'detail' }, detail));
}

function hostTiles(host) {
  const memPct = host.memory_total_bytes ? (host.memory_used_bytes / host.memory_total_bytes) * 100 : 0;
  const diskUsed = host.disk_total_bytes - host.disk_available_bytes;
  const diskPct = host.disk_total_bytes ? (diskUsed / host.disk_total_bytes) * 100 : 0;
  const load = host.load_average.map((l) => l.toFixed(2));
  const loadPct = host.cpu_count ? (host.load_average[0] / host.cpu_count) * 100 : 0;
  return [
    tile('CPU', `${host.cpu_percent.toFixed(0)}%`, host.cpu_percent, `${host.cpu_count} cores · ${host.hostname}`, 'cpu'),
    tile('Memory', `${memPct.toFixed(0)}%`, memPct,
      `${fmtBytes(host.memory_used_bytes)} of ${fmtBytes(host.memory_total_bytes)}${host.swap_total_bytes ? ` · swap ${fmtBytes(host.swap_used_bytes)}` : ''}`, 'memory'),
    tile('Disk', `${diskPct.toFixed(0)}%`, diskPct,
      `${fmtBytes(host.disk_available_bytes)} free on ${host.disk_mount_point || '/'}`, 'disk'),
    tile('Load', load[0], loadPct, `${load.join(' · ')} (1, 5, 15 min) · up ${fmtDuration(host.uptime_secs * 1000)}`, 'load'),
  ];
}

function processButtons(project, after, { small = true } = {}) {
  const cls = small ? 'btn small' : 'btn';
  const p = project.process;
  const running = p && ['Running', 'Backoff'].includes(p.status);
  const buttons = [];
  if (!project.current_release) return buttons;
  const act = (action) => async (e) => {
    const btn = e.currentTarget;
    if (action !== 'start') {
      const ok = await confirmDialog({
        title: `${action === 'stop' ? 'Stop' : 'Restart'} ${project.name}?`,
        message: action === 'stop'
          ? 'The app goes offline until you start it again.'
          : 'The app is briefly unavailable while its process restarts.',
        confirmLabel: action === 'stop' ? 'Stop' : 'Restart',
        danger: action === 'stop',
      });
      if (!ok) return;
    }
    await run(btn, () => api('POST', `/processes/${enc(project.name)}/${action}`), `${project.name}: ${action} requested`);
    if (after) after();
  };
  if (running) {
    buttons.push(h('button', { class: cls, type: 'button', onclick: act('restart'), 'data-action': 'restart' }, 'Restart'));
    buttons.push(h('button', { class: `${cls} danger`, type: 'button', onclick: act('stop'), 'data-action': 'stop' }, 'Stop'));
  } else {
    buttons.push(h('button', { class: cls, type: 'button', onclick: act('start'), 'data-action': 'start' }, 'Start'));
  }
  return buttons;
}

async function quickDeploy(project, btn) {
  const res = await run(btn, () => api('POST', `/projects/${enc(project.name)}/deploy`, {}), `Deploying ${project.name}`);
  if (res) go(`/apps/${enc(project.name)}/deploy`);
}

function appCard(project, activeDeployment, reload) {
  const p = project.process;
  return h('article', { class: 'card app-card', 'data-app': project.name },
    h('div', { class: 'card-body' },
      h('div', { class: 'title' },
        h('a', { href: `#/apps/${enc(project.name)}` }, project.name),
        h('span', { class: 'spacer' }),
        activeDeployment ? deploymentPill(activeDeployment.status) : null,
        processPill(project)),
      h('div', { class: 'source muted truncate', title: sourceText(project) }, sourceText(project)),
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Version'), h('dd', { class: 'truncate', title: project.current_release || '' }, project.current_release || '–')),
        h('div', {}, h('dt', {}, 'Uptime'), h('dd', {}, uptime(p))),
        h('div', {}, h('dt', {}, 'Restarts'), h('dd', {}, p ? p.restart_count : '–')))),
    h('div', { class: 'actions' },
      h('button', { class: 'btn small primary', type: 'button', 'data-action': 'deploy', onclick: (e) => quickDeploy(project, e.currentTarget) }, 'Deploy'),
      processButtons(project, reload),
      h('a', { class: 'btn small ghost', href: `#/apps/${enc(project.name)}/logs` }, 'Logs')));
}

// ----------------------------------------------------------------------
// App detail
// ----------------------------------------------------------------------

const APP_TABS = [
  ['deploy', 'Deploy'],
  ['releases', 'Releases'],
  ['logs', 'Logs'],
  ['settings', 'Settings'],
  ['activity', 'Activity'],
];

async function appPage(main, ctx, name, tab = 'deploy') {
  if (!APP_TABS.some(([t]) => t === tab)) tab = 'deploy';
  let project = await api('GET', `/projects/${enc(name)}`);
  if (!ctx.alive()) return;
  const header = h('div');
  const body = h('div', { id: 'tab' });
  const tabs = h('nav', { class: 'tabs', 'aria-label': 'App sections' },
    APP_TABS.map(([t, label], i) => h('a', {
      href: `#/apps/${enc(project.name)}/${t}`,
      'aria-current': t === tab ? 'page' : null,
      title: `${label} (${i + 1})`,
    }, label)));

  const drawHeader = () => {
    const p = project.process;
    replace(header, h('div', { class: 'page-header' },
      h('h1', {}, project.name),
      processPill(project),
      h('div', { class: 'spacer' }),
      processButtons(project, refreshProject, { small: false }),
      h('div', { class: 'subtitle muted small' },
        [
          project.current_release ? `Version ${project.current_release}` : 'Not deployed yet',
          p && p.status === 'Running' ? `up ${uptime(p)}` : null,
          p ? `${p.restart_count} restart${p.restart_count === 1 ? '' : 's'}` : null,
          p && p.pid ? `pid ${p.pid}` : null,
          sourceText(project),
        ].filter(Boolean).join(' · '))));
  };
  const refreshProject = async () => {
    try {
      project = await api('GET', `/projects/${enc(project.id)}`);
      if (ctx.alive()) drawHeader();
    } catch {
      /* shown on next load */
    }
  };
  drawHeader();
  append(main, h('a', { class: 'small', href: '#/' }, '← All apps'), header, tabs, body);

  const reloadHeader = debounce(refreshProject, 250);
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync' || e.payload?.project_id === project.id) reloadHeader();
  });
  every(ctx, 30000, refreshProject);
  ctx.cleanup(reloadHeader.cancel);

  const tabCtx = { ...ctx, project: () => project, refreshProject };
  switch (tab) {
    case 'releases': return releasesTab(body, tabCtx);
    case 'logs': return logsTab(body, tabCtx);
    case 'settings': return settingsTab(body, tabCtx);
    case 'activity': return activityTab(body, tabCtx);
    default: return deployTab(body, tabCtx);
  }
}

// --- Deploy tab --------------------------------------------------------

async function deployTab(body, ctx) {
  const project = ctx.project();
  const pipelineEl = h('div');
  const branch = h('input', { id: 'deploy-branch', placeholder: project.branch, class: 'mono' });
  const commit = h('input', { id: 'deploy-commit', placeholder: 'latest', class: 'mono' });
  const strategy = h('select', { id: 'deploy-strategy' },
    h('option', { value: 'GracefulSwitch' }, 'Graceful switch (zero downtime)'),
    h('option', { value: 'Immediate' }, 'Immediate'));
  const submit = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'deploy' }, 'Deploy');
  const gitSource = Boolean(project.repository_url);

  const form = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      const req = { strategy: strategy.value };
      if (branch.value.trim()) req.branch = branch.value.trim();
      if (commit.value.trim()) req.commit = commit.value.trim();
      const res = await run(submit, () => api('POST', `/projects/${enc(project.name)}/deploy`, req), 'Deployment queued');
      if (res) showDeployment(res.deployment_id);
    },
  },
  h('div', { class: 'card-header' }, h('h2', {}, 'New deployment')),
  h('div', { class: 'card-body' },
    h('div', { class: 'row' },
      gitSource ? h('div', { class: 'field' }, h('label', { for: 'deploy-branch' }, 'Branch'), branch) : null,
      gitSource ? h('div', { class: 'field' }, h('label', { for: 'deploy-commit' }, 'Commit'), commit, h('span', { class: 'hint' }, 'A commit SHA on that branch; blank for its latest commit')) : null,
      h('div', { class: 'field' }, h('label', { for: 'deploy-strategy' }, 'Strategy'), strategy)),
    gitSource ? null : h('p', { class: 'muted small' }, `Copies the code in ${project.source_dir || 'the source directory'}.`),
    h('div', { class: 'form-actions' }, submit)));

  append(body, h('div', { class: 'grid' }, form, pipelineEl));

  let current = null;
  const showDeployment = (id) => {
    if (current === id) return;
    current = id;
    if (!id) {
      replace(pipelineEl, h('div', { class: 'card empty' }, h('h2', {}, 'No deployments yet'), h('p', {}, 'Start one above; its progress appears here live.')));
      return;
    }
    replace(pipelineEl, pipelineCard(ctx, id, { link: true }));
  };
  const loadLatest = async () => {
    const list = await api('GET', `/deployments?project=${enc(project.id)}`);
    if (!ctx.alive()) return;
    const latest = list[list.length - 1];
    showDeployment(latest ? latest.id : null);
  };
  await loadLatest();
  // A deployment started elsewhere (CLI, webhook, schedule) takes over the view.
  onEvents(ctx, (e) => {
    if (e.event_type === 'DeploymentQueued' && e.payload?.project_id === project.id) showDeployment(e.payload.deployment_id);
  });
}

// --- Pipeline view (deploy tab and deployment detail) -------------------

const STAGES = ['Clone', 'Install', 'Build', 'Test', 'Package', 'Verify', 'Promote'];

function stageStates(deployment, events) {
  const stages = Object.fromEntries(STAGES.map((s) => [s, { status: 'pending', start: null, end: null, detail: null }]));
  for (const e of events) {
    if (!e.event_type.startsWith('BuildStage')) continue;
    const s = stages[e.payload?.stage];
    if (!s) continue;
    switch (e.payload.status) {
      case 'Started':
        s.status = 'running';
        s.start = e.created_at;
        break;
      case 'Success':
        s.status = 'success';
        s.end = e.created_at;
        break;
      case 'Skipped':
        s.status = 'skipped';
        s.end = e.created_at;
        break;
      case 'Failed':
        s.status = 'failed';
        s.end = e.created_at;
        break;
      default:
        break;
    }
    if (e.payload.detail) s.detail = e.payload.detail;
  }
  const finished = !isActiveDeployment(deployment);
  if (finished && deployment.status !== 'Success') {
    // Whatever was still running is where it stopped.
    for (const name of STAGES) {
      if (stages[name].status === 'running') stages[name].status = 'failed';
    }
    const failedAt = deployment.stage && stages[deployment.stage];
    if (failedAt && failedAt.status === 'pending') failedAt.status = 'failed';
  }
  return stages;
}

const STAGE_MARK = { pending: '·', running: '…', success: '✓', skipped: '–', failed: '✕' };

function pipelineCard(ctx, id, { link = false } = {}) {
  const card = h('section', { class: 'card', 'data-deployment': id });
  const logPre = h('pre', { class: 'build-log', 'data-build-log': '', tabindex: '0', 'aria-label': 'Build log' });
  let deployment = null;
  let stopped = false;
  ctx.cleanup(() => {
    stopped = true;
  });

  const draw = (events, lines) => {
    const stages = stageStates(deployment, events);
    const duration = durationBetween(deployment.created_at, deployment.completed_at);
    const stick = logPre.scrollHeight - logPre.scrollTop - logPre.clientHeight < 40;
    replace(logPre, lines.length ? lines.join('\n') : h('span', { class: 'notice' }, 'No build output yet.'));
    if (stick) logPre.scrollTop = logPre.scrollHeight;
    replace(card,
      h('div', { class: 'card-header' },
        h('h2', {}, deployment.version ? `Release ${deployment.version}` : 'Deployment'),
        deploymentPill(deployment.status),
        h('span', { class: 'spacer' }),
        h('span', { class: 'muted small' },
          [deployment.strategy, deployment.trigger, `started ${fmtAgo(deployment.created_at)}`, `took ${fmtDuration(duration)}`].filter(Boolean).join(' · ')),
        link ? h('a', { class: 'btn small ghost', href: `#/deployments/${deployment.id}` }, 'Details') : null),
      h('div', { class: 'card-body' },
        h('ol', { class: 'pipeline', 'aria-label': 'Pipeline stages' },
          STAGES.map((name) => {
            const s = stages[name];
            const took = s.start && s.end ? fmtDuration(durationBetween(s.start, s.end)) : s.status === 'running' ? 'running' : '';
            return h('li', { class: `stage ${s.status}`, 'data-stage': name, 'data-state': s.status, title: s.detail || '' },
              h('span', { class: 'mark', 'aria-hidden': 'true' }, STAGE_MARK[s.status]),
              h('div', { class: 'name' }, name),
              h('div', { class: 'time' }, took || ' '));
          })),
        deployment.error ? h('div', { class: 'alert outcome', role: 'alert', 'data-error': '' }, deployment.error) : null,
        deployment.status === 'Success' ? h('div', { class: 'alert ok outcome' }, `${deployment.version} is live.`) : null,
        h('details', { class: 'outcome', open: isActiveDeployment(deployment) || deployment.status !== 'Success' },
          h('summary', {}, 'Build log'),
          logPre)));
  };

  const load = async () => {
    const [d, events, log] = await Promise.all([
      api('GET', `/deployments/${id}`),
      api('GET', `/deployments/${id}/events`),
      api('GET', `/deployments/${id}/log?lines=1000`),
    ]);
    if (stopped) return;
    deployment = d;
    draw(events, log.lines);
  };
  const reload = debounce(() => load().catch(() => {}), 150);
  load().catch((err) => {
    if (!stopped) replace(card, errorBox(err));
  });
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync' || e.payload?.deployment_id === id) reload();
  });
  // Build output is written to a file, not to events: poll while it runs.
  every(ctx, 1500, () => {
    if (deployment && isActiveDeployment(deployment)) reload();
  });
  ctx.cleanup(reload.cancel);
  return card;
}

// --- Releases tab ------------------------------------------------------

async function releasesTab(body, ctx) {
  const project = ctx.project();
  const load = async () => {
    const releases = (await api('GET', `/projects/${enc(project.id)}/releases`)).reverse();
    if (!ctx.alive()) return;
    if (releases.length === 0) {
      replace(body, h('div', { class: 'card empty' }, h('h2', {}, 'No releases yet'), h('p', {}, 'Each successful build becomes a release you can roll back to.')));
      return;
    }
    replace(body, h('div', { class: 'card table-wrap' },
      h('table', { class: 'stack' },
        h('thead', {}, h('tr', {}, ['Version', 'Status', 'Commit', 'Built', ''].map((t) => h('th', {}, t)))),
        h('tbody', {}, releases.map((r) => h('tr', { 'data-release': r.version },
          h('td', { 'data-label': 'Version', class: 'mono' }, r.version),
          h('td', { 'data-label': 'Status' }, releasePill(r.status)),
          h('td', { 'data-label': 'Commit', class: 'truncate', title: r.commit_message },
            r.commit_sha ? h('span', { class: 'tag' }, r.commit_sha.slice(0, 8)) : null, ' ', r.commit_message ? r.commit_message.split('\n')[0] : ''),
          h('td', { 'data-label': 'Built' }, timeEl(r.created_at)),
          h('td', { class: 'actions' },
            ['Built', 'Inactive'].includes(r.status)
              ? h('button', {
                class: 'btn small',
                type: 'button',
                'data-action': 'rollback',
                onclick: async (e) => {
                  const ok = await confirmDialog({
                    title: `Roll back to ${r.version}?`,
                    message: `${project.name} switches to this release after it passes its health check. The current release stays available.`,
                    confirmLabel: 'Roll back',
                  });
                  if (!ok) return;
                  await run(e.currentTarget, () => api('POST', `/projects/${enc(project.id)}/rollback`, { version: r.version }), (res) => `${res.version} is live`);
                  load().catch(() => {});
                },
              }, 'Roll back')
              : null)))))));
  };
  await load();
  const reload = debounce(() => load().catch(() => {}), 250);
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync' || (e.payload?.project_id === project.id && /^(Release|Rollback|Deployment)/.test(e.event_type))) reload();
  });
  ctx.cleanup(reload.cancel);
}

// --- Logs tab ----------------------------------------------------------

async function logsTab(body, ctx) {
  const project = ctx.project();
  if (!project.process && !project.current_release) {
    append(body, h('div', { class: 'card empty' }, h('h2', {}, 'No logs yet'), h('p', {}, 'Logs appear once the app has been deployed.')));
    return;
  }
  append(body, logViewer(ctx, project.name));
}

const MAX_LOG_LINES = 5000;

function logViewer(ctx, target) {
  const lines = [];
  let pending = [];
  let paused = false;
  let query = '';
  let streamFilter = 'all';
  let source = null;
  let retry = null;

  const status = h('span', { class: 'pill warn live', 'data-log-status': '' }, 'Connecting');
  const view = h('div', { class: 'log', role: 'log', 'aria-label': 'Process output', tabindex: '0', 'data-log': '' });
  const follow = h('input', { type: 'checkbox', id: 'log-follow', checked: true });
  const pauseBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'pause' }, 'Pause');
  const search = h('input', { type: 'search', placeholder: 'Search logs  /', 'data-search': '', 'aria-label': 'Search logs' });
  const filter = h('select', { 'aria-label': 'Stream' },
    h('option', { value: 'all' }, 'All output'),
    h('option', { value: 'out' }, 'stdout'),
    h('option', { value: 'err' }, 'stderr'),
    h('option', { value: 'sys' }, 'Supervisor'));
  const counter = h('span', { class: 'muted small' });

  const visible = (l) => (streamFilter === 'all' || l.stream === streamFilter) && (!query || l.line.toLowerCase().includes(query));

  const lineEl = (l) => {
    const text = h('span', { class: l.stream === 'err' ? 'err' : l.stream === 'sys' ? 'sys' : '' });
    if (query) {
      const lower = l.line.toLowerCase();
      let at = 0;
      for (let i = lower.indexOf(query); i !== -1; i = lower.indexOf(query, at)) {
        append(text, [l.line.slice(at, i), h('mark', {}, l.line.slice(i, i + query.length))]);
        at = i + query.length;
      }
      append(text, [l.line.slice(at)]);
    } else {
      text.textContent = l.line;
    }
    const t = new Date(l.timestamp);
    return h('div', { class: 'line' }, h('span', { class: 'ts' }, Number.isNaN(t.getTime()) ? '' : t.toLocaleTimeString([], { hour12: false })), text);
  };

  const updateCounter = () => {
    counter.textContent = paused && pending.length ? `${pending.length} new lines while paused` : `${lines.length} lines`;
  };

  const scroll = () => {
    if (follow.checked) view.scrollTop = view.scrollHeight;
  };

  const redraw = () => {
    const shown = lines.filter(visible);
    replace(view, shown.length ? shown.map(lineEl) : h('div', { class: 'notice' }, lines.length ? 'No lines match.' : 'Waiting for output…'));
    updateCounter();
    scroll();
  };

  const add = (batch) => {
    lines.push(...batch);
    const overflow = lines.length - MAX_LOG_LINES;
    if (overflow > 0) {
      lines.splice(0, overflow);
      redraw();
      return;
    }
    const shown = batch.filter(visible);
    if (shown.length) {
      if (view.firstChild && view.firstChild.classList?.contains('notice')) view.replaceChildren();
      append(view, shown.map(lineEl));
    }
    updateCounter();
    scroll();
  };

  const setStatus = (text, kind) => {
    status.className = `pill ${kind}`;
    status.textContent = text;
  };

  // Batch DOM work for chatty processes.
  let queued = [];
  let frame = null;
  const receive = (line) => {
    if (paused) {
      pending.push(line);
      updateCounter();
      return;
    }
    queued.push(line);
    if (!frame) {
      frame = requestAnimationFrame(() => {
        frame = null;
        const batch = queued;
        queued = [];
        add(batch);
      });
    }
  };

  const connect = () => {
    // Each connection replays recent history, so start from a clean slate.
    lines.length = 0;
    pending = [];
    queued = [];
    redraw();
    setStatus('Connecting', 'warn live');
    source = new EventSource(`/api/v1/processes/${enc(target)}/logs/stream?lines=500`);
    source.addEventListener('open', () => setStatus('Live', 'ok live'));
    source.addEventListener('log', (m) => {
      try {
        receive(JSON.parse(m.data));
      } catch {
        /* ignore malformed */
      }
    });
    source.addEventListener('end', () => {
      source.close();
      setStatus('Not running', '');
    });
    source.addEventListener('error', () => {
      if (source.readyState === EventSource.CLOSED && status.textContent === 'Not running') return;
      source.close();
      setStatus('Reconnecting', 'warn live');
      clearTimeout(retry);
      retry = setTimeout(() => {
        // A 401 here signs the page out; otherwise reconnect.
        api('GET', '/auth/session').then(() => ctx.alive() && connect()).catch(() => {});
      }, 2000);
    });
  };

  pauseBtn.addEventListener('click', () => {
    paused = !paused;
    pauseBtn.textContent = paused ? 'Resume' : 'Pause';
    pauseBtn.classList.toggle('primary', paused);
    if (!paused && pending.length) {
      const batch = pending;
      pending = [];
      add(batch);
    }
    updateCounter();
  });
  search.addEventListener('input', debounce(() => {
    query = search.value.trim().toLowerCase();
    redraw();
  }, 120));
  filter.addEventListener('change', () => {
    streamFilter = filter.value;
    redraw();
  });
  follow.addEventListener('change', scroll);
  // Scrolling up stops following; scrolling to the bottom resumes it.
  view.addEventListener('scroll', () => {
    follow.checked = view.scrollHeight - view.scrollTop - view.clientHeight < 30;
  });

  const download = () => {
    const text = lines.map((l) => `${l.timestamp} [${l.stream}] ${l.line}`).join('\n');
    const url = URL.createObjectURL(new Blob([`${text}\n`], { type: 'text/plain' }));
    const a = h('a', { href: url, download: `${target}-${new Date().toISOString().replace(/[:.]/g, '-')}.log` });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  connect();
  ctx.cleanup(() => {
    if (source) source.close();
    clearTimeout(retry);
    if (frame) cancelAnimationFrame(frame);
  });

  return h('div', {},
    h('div', { class: 'log-toolbar' },
      status,
      search,
      filter,
      h('label', { class: 'small', for: 'log-follow' }, follow, ' Follow'),
      pauseBtn,
      h('span', { class: 'spacer' }),
      counter,
      h('button', { class: 'btn small', type: 'button', onclick: () => { lines.length = 0; pending = []; redraw(); } }, 'Clear'),
      h('button', { class: 'btn small', type: 'button', 'data-action': 'download', onclick: download }, 'Download')),
    view);
}

// --- Settings tab ------------------------------------------------------

const SETTING_FIELDS = [
  ['install_command', 'Install command', 'e.g. npm ci'],
  ['build_command', 'Build command', 'e.g. npm run build'],
  ['test_command', 'Test command', 'e.g. npm test'],
  ['start_command', 'Start command', 'e.g. node server.js'],
];

function settingsInputs(values, placeholders = {}) {
  const inputs = {};
  const fields = SETTING_FIELDS.map(([key, label, example]) => {
    inputs[key] = h('input', { id: `set-${key}`, name: key, class: 'mono', value: values[key] ?? '', placeholder: placeholders[key] ?? example });
    return h('div', { class: 'field' }, h('label', { for: `set-${key}` }, label), inputs[key]);
  });
  inputs.port = h('input', { id: 'set-port', name: 'port', type: 'number', min: 1, max: 65535, value: values.port ?? '', placeholder: placeholders.port ?? '' });
  inputs.health_check_url = h('input', { id: 'set-health', name: 'health_check_url', class: 'mono', value: values.health_check_url ?? '', placeholder: placeholders.health_check_url ?? 'http://127.0.0.1:3000/health' });
  // Keep the health check pointed at the app's port when the port changes.
  let lastPort = inputs.port.value || placeholders.port || '';
  inputs.port.addEventListener('input', () => {
    const next = inputs.port.value;
    const url = inputs.health_check_url.value;
    const re = /^(https?:\/\/(?:127\.0\.0\.1|localhost)):(\d+)(\/.*)?$/;
    const m = url.match(re);
    if (m && lastPort && m[2] === String(lastPort) && next) {
      inputs.health_check_url.value = `${m[1]}:${next}${m[3] || ''}`;
    }
    lastPort = next;
  });
  const layout = [
    ...fields,
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'set-port' }, 'Port'), inputs.port),
      h('div', { class: 'field' }, h('label', { for: 'set-health' }, 'Health check URL'), inputs.health_check_url,
        h('span', { class: 'hint' }, 'A new release goes live only once this URL responds (any status below 500).'))),
  ];
  return { inputs, layout };
}

async function settingsTab(body, ctx) {
  const project = ctx.project();
  const s = project.settings || {};
  const { inputs, layout } = settingsInputs(s);
  const processOnly = h('input', { type: 'checkbox', id: 'set-process-only', checked: s.health_check_url === '' });
  const syncHealth = () => {
    inputs.health_check_url.disabled = processOnly.checked;
  };
  processOnly.addEventListener('change', syncHealth);
  syncHealth();
  const saveBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'save-settings' }, 'Save settings');
  const detectBtn = h('button', { class: 'btn', type: 'button' }, 'Show detected defaults');
  detectBtn.addEventListener('click', async () => {
    const detected = await run(detectBtn, () => api('POST', '/detect', project.repository_url
      ? { repository_url: project.repository_url, branch: project.branch }
      : { source_dir: project.source_dir }));
    if (!detected) return;
    for (const [key] of SETTING_FIELDS) inputs[key].placeholder = detected[key] || '(none)';
    inputs.port.placeholder = detected.port ?? '';
    inputs.health_check_url.placeholder = detected.health_check_url || '(process check only)';
    toast(`Detected a ${detected.runtime} app; defaults shown as placeholders`);
  });

  const settingsForm = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      const req = {};
      for (const [key] of SETTING_FIELDS) {
        const v = inputs[key].value.trim();
        if (v) req[key] = v;
      }
      if (inputs.port.value) req.port = Number(inputs.port.value);
      if (processOnly.checked) req.health_check_url = '';
      else if (inputs.health_check_url.value.trim()) req.health_check_url = inputs.health_check_url.value.trim();
      await run(saveBtn, () => api('PUT', `/projects/${enc(project.id)}/settings`, req), 'Settings saved; they apply to the next deployment');
      ctx.refreshProject();
    },
  },
  h('div', { class: 'card-header' }, h('h2', {}, 'Build & run'), h('span', { class: 'spacer' }), detectBtn),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, 'Leave a field blank to use the value from aegis.toml or auto-detection. Changes take effect on the next deployment; older releases keep the settings they were built with.'),
    layout,
    h('label', { class: 'small', for: 'set-process-only' }, processOnly, ' No HTTP health check (healthy while the process stays up)'),
    h('div', { class: 'form-actions' }, saveBtn)));

  // Daily deploy.
  const sched = project.schedule;
  const time = h('input', { id: 'sched-time', type: 'time', value: sched ? `${String(sched.hour).padStart(2, '0')}:${String(sched.minute).padStart(2, '0')}` : '' });
  const schedBranch = h('input', { id: 'sched-branch', class: 'mono', value: sched?.branch ?? '', placeholder: project.branch });
  const schedBtn = h('button', { class: 'btn', type: 'submit' }, 'Save schedule');
  const scheduleForm = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      const [hour, minute] = time.value.split(':').map(Number);
      if (Number.isNaN(hour)) {
        toast('Pick a time', 'bad');
        return;
      }
      const req = { hour, minute: minute || 0 };
      if (schedBranch.value.trim()) req.branch = schedBranch.value.trim();
      await run(schedBtn, () => api('PUT', `/projects/${enc(project.id)}/schedule`, req), 'Schedule saved');
      ctx.refreshProject();
    },
  },
  h('div', { class: 'card-header' }, h('h2', {}, 'Daily deploy')),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, sched ? `Deploys every day at ${time.value} UTC.` : 'Not scheduled.'),
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'sched-time' }, 'Time (UTC)'), time),
      project.repository_url ? h('div', { class: 'field' }, h('label', { for: 'sched-branch' }, 'Branch'), schedBranch) : null),
    h('div', { class: 'form-actions' }, schedBtn)));

  // Webhook.
  const hookOut = h('div');
  const hookBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'webhook' }, 'Generate webhook secret');
  hookBtn.addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: 'Generate a new webhook secret?',
      message: 'If this app already has a secret, it stops working; update the webhook on GitHub with the new one.',
      confirmLabel: 'Generate',
    });
    if (!ok) return;
    const res = await run(hookBtn, () => api('POST', `/projects/${enc(project.id)}/webhook`));
    if (!res) return;
    replace(hookOut, h('div', { class: 'grid' },
      h('div', { class: 'alert warn' }, 'Copy the secret now; it is not shown again.'),
      h('div', {}, h('div', { class: 'small muted' }, 'Payload URL'), secretBox(`${location.origin}${res.path}`, 'Payload URL')),
      h('div', {}, h('div', { class: 'small muted' }, 'Secret'), secretBox(res.secret, 'Webhook secret')),
      h('div', { class: 'small muted' }, `Content type ${res.content_type}; events: ${res.events.join(', ')}.`)));
  });
  const hookCard = h('section', { class: 'card' },
    h('div', { class: 'card-header' }, h('h2', {}, 'GitHub webhook')),
    h('div', { class: 'card-body' },
      project.repository_url
        ? [h('p', { class: 'muted small' }, `Deploy automatically when ${project.branch} is pushed. The server must be reachable from GitHub.`), hookBtn, hookOut]
        : h('p', { class: 'muted small' }, 'Webhooks need a git repository as the source.')));

  const info = h('section', { class: 'card' },
    h('div', { class: 'card-header' }, h('h2', {}, 'About')),
    h('div', { class: 'card-body' },
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Project id'), h('dd', { class: 'mono small truncate', title: project.id }, project.id)),
        h('div', {}, h('dt', {}, 'Runtime'), h('dd', {}, project.runtime || 'auto')),
        h('div', {}, h('dt', {}, 'Created'), h('dd', {}, timeEl(project.created_at)))),
      h('p', { class: 'muted small' }, 'Source: ', sourceText(project))));

  append(body, h('div', { class: 'grid' }, settingsForm, h('div', { class: 'grid two' }, scheduleForm, hookCard), info));
}

// --- Activity tab ------------------------------------------------------

async function activityTab(body, ctx) {
  const project = ctx.project();
  const list = h('ul', { class: 'events', 'data-events': '' });
  append(body, h('div', { class: 'card' }, list));
  const load = async () => {
    const events = await api('GET', `/events?project=${enc(project.id)}&limit=200`);
    if (!ctx.alive()) return;
    replace(list, events.length ? events.reverse().map(eventItem) : h('li', { class: 'empty' }, 'Nothing has happened yet.'));
  };
  await load();
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync') {
      load().catch(() => {});
    } else if (e.payload?.project_id === project.id) {
      if (list.firstChild?.classList?.contains('empty')) list.replaceChildren();
      list.prepend(eventItem(e));
    }
  });
}

// ----------------------------------------------------------------------
// Events (audit log)
// ----------------------------------------------------------------------

function projectName(id) {
  const p = state.projects.find((x) => x.id === id);
  return p ? p.name : id ? shortId(id) : null;
}

function describeEvent(e) {
  const p = e.payload || {};
  const bits = [];
  const app = projectName(p.project_id);
  if (app) bits.push(app);
  switch (e.event_type) {
    case 'DeploymentQueued':
      bits.push([p.branch, p.commit_sha && p.commit_sha.slice(0, 8), p.trigger_source].filter(Boolean).join(' · '));
      break;
    case 'DeploymentFailed':
      bits.push(`${p.stage || ''}: ${p.reason || ''}`);
      break;
    case 'DeploymentRolledBack':
      bits.push(`back to ${p.target_version}: ${p.reason || ''}`);
      break;
    case 'ReleasePromoted':
      bits.push(`${p.version} live${p.previous_version ? ` (was ${p.previous_version})` : ''}`);
      break;
    case 'RollbackRequested':
    case 'RollbackCompleted':
      bits.push(`${p.from_version || '?'} → ${p.to_version || p.target_version || '?'}`);
      break;
    case 'RollbackFailed':
      bits.push(`${p.target_version}: ${p.reason || ''}`);
      break;
    case 'ProcessStarted':
      bits.push(`pid ${p.pid}${p.release_version ? ` · ${p.release_version}` : ''}${p.restart_count ? ` · restart #${p.restart_count}` : ''}`);
      break;
    case 'ProcessCrashed':
      bits.push(`exit ${p.exit_code ?? '?'}${p.will_restart ? `, restarting in ${fmtDuration(p.restart_delay_ms)}` : ''}`);
      break;
    case 'ProcessFailed':
      bits.push(p.reason);
      break;
    case 'ProcessStopped':
      bits.push(p.reason);
      break;
    case 'UserLoggedIn':
    case 'UserLoginFailed':
      bits.push(`${p.username}${p.ip ? ` from ${p.ip}` : ''}`);
      break;
    case 'UserLoggedOut':
    case 'PasswordChanged':
    case 'PasswordReset':
      bits.push(p.username);
      break;
    case 'ApiTokenCreated':
    case 'ApiTokenRevoked':
      bits.push(p.name ? `${p.name}${p.scope ? ` (${p.scope})` : ''}` : shortId(p.token_id));
      break;
    default:
      if (e.event_type.startsWith('BuildStage')) bits.push(`${p.status}${p.detail ? ` · ${p.detail}` : ''}`);
      else if (p.version) bits.push(p.version);
  }
  return bits.filter(Boolean).join(' — ');
}

function eventKind(type) {
  if (/Failed|Crashed/.test(type)) return 'bad';
  if (/RolledBack|LoginFailed/.test(type)) return 'warn';
  if (/Completed|Succeeded|Promoted|LoggedIn/.test(type)) return 'ok';
  return '';
}

function eventItem(e) {
  const actor = e.payload?.actor;
  return h('li', { 'data-event-type': e.event_type },
    h('span', { class: 'muted small nowrap' }, timeEl(e.created_at)),
    h('div', { class: 'what' },
      h('div', {}, h('span', { class: `type ${eventKind(e.event_type) ? `pill plain ${eventKind(e.event_type)}` : ''}` }, e.event_type), ' ',
        h('span', { class: 'muted' }, describeEvent(e))),
      h('details', {}, h('summary', {}, 'Details'), h('pre', {}, JSON.stringify(e.payload, null, 2)))),
    h('span', { class: 'muted small nowrap' }, actor ? `by ${actor}` : ''));
}

const EVENT_CATEGORIES = [
  ['all', 'All events', () => true],
  ['deploy', 'Deployments & builds', (t) => /^(Deployment|BuildStage|Release|Rollback|Scheduled)/.test(t)],
  ['process', 'Processes', (t) => t.startsWith('Process')],
  ['access', 'Access & security', (t) => /^(User|Password|ApiToken|Webhook)/.test(t)],
  ['config', 'App configuration', (t) => /^(Project|Schedule)/.test(t)],
];

async function eventsPage(main, ctx) {
  if (!state.projects.length) state.projects = await api('GET', '/projects');
  if (!ctx.alive()) return;
  const projectSel = h('select', { 'aria-label': 'App' }, h('option', { value: '' }, 'All apps'),
    state.projects.map((p) => h('option', { value: p.id }, p.name)));
  const categorySel = h('select', { 'aria-label': 'Category' }, EVENT_CATEGORIES.map(([v, label]) => h('option', { value: v }, label)));
  const limitSel = h('select', { 'aria-label': 'How many' }, [100, 500, 1000].map((n) => h('option', { value: n }, `Last ${n}`)));
  const search = h('input', { type: 'search', placeholder: 'Filter by text  /', 'data-search': '', 'aria-label': 'Filter events' });
  const live = h('input', { type: 'checkbox', id: 'events-live', checked: true });
  const list = h('ul', { class: 'events', 'data-events': '' });
  const count = h('span', { class: 'muted small' });
  let events = [];

  const matches = (e) => {
    const [, , test] = EVENT_CATEGORIES.find(([v]) => v === categorySel.value);
    if (!test(e.event_type)) return false;
    if (projectSel.value && e.payload?.project_id !== projectSel.value) return false;
    const q = search.value.trim().toLowerCase();
    return !q || e.event_type.toLowerCase().includes(q) || JSON.stringify(e.payload).toLowerCase().includes(q);
  };
  const draw = () => {
    const shown = events.filter(matches);
    count.textContent = `${shown.length} of ${events.length}`;
    replace(list, shown.length ? shown.map(eventItem) : h('li', { class: 'empty' }, 'No matching events.'));
  };
  const load = async () => {
    const q = projectSel.value ? `project=${enc(projectSel.value)}&` : '';
    events = (await api('GET', `/events?${q}limit=${limitSel.value}`)).reverse();
    if (ctx.alive()) draw();
  };

  projectSel.addEventListener('change', () => load().catch((err) => toast(err.message, 'bad')));
  limitSel.addEventListener('change', () => load().catch((err) => toast(err.message, 'bad')));
  categorySel.addEventListener('change', draw);
  search.addEventListener('input', debounce(draw, 120));

  append(main,
    pageHeader('Events', count),
    h('div', { class: 'card' },
      h('div', { class: 'card-header filters' }, projectSel, categorySel, limitSel, search,
        h('label', { class: 'small', for: 'events-live' }, live, ' Live')),
      list));
  await load();
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync') {
      load().catch(() => {});
      return;
    }
    if (!live.checked) return;
    events.unshift(e);
    if (matches(e)) {
      if (list.firstChild?.classList?.contains('empty')) list.replaceChildren();
      list.prepend(eventItem(e));
      count.textContent = `${events.filter(matches).length} of ${events.length}`;
    }
  });
}

// ----------------------------------------------------------------------
// Deployments timeline
// ----------------------------------------------------------------------

async function deploymentsPage(main, ctx) {
  const projectSel = h('select', { 'aria-label': 'App' }, h('option', { value: '' }, 'All apps'));
  const statusSel = h('select', { 'aria-label': 'Status' },
    [['', 'Any status'], ['active', 'In progress'], ['Success', 'Succeeded'], ['Failed', 'Failed'], ['RolledBack', 'Rolled back']]
      .map(([v, l]) => h('option', { value: v }, l)));
  const tableBody = h('tbody');
  const wrap = h('div', { class: 'card table-wrap' },
    h('div', { class: 'card-header filters' }, projectSel, statusSel),
    h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Status', 'App', 'Release', 'Trigger', 'Stage', 'Started', 'Duration'].map((t) => h('th', {}, t)))),
      tableBody));
  append(main, pageHeader('Deployments'), wrap);
  let deployments = [];

  const draw = () => {
    const shown = deployments.filter((d) => (!projectSel.value || d.project_id === projectSel.value)
      && (!statusSel.value || (statusSel.value === 'active' ? isActiveDeployment(d) : d.status === statusSel.value)));
    if (!shown.length) {
      replace(tableBody, h('tr', {}, h('td', { colspan: 7, class: 'empty' }, deployments.length ? 'No deployments match.' : 'No deployments yet.')));
      return;
    }
    replace(tableBody, shown.map((d) => h('tr', {
      class: 'clickable',
      'data-deployment': d.id,
      tabindex: '0',
      onclick: () => go(`/deployments/${d.id}`),
      onkeydown: (e) => { if (e.key === 'Enter') go(`/deployments/${d.id}`); },
    },
    h('td', { 'data-label': 'Status' }, deploymentPill(d.status)),
    h('td', { 'data-label': 'App' }, projectName(d.project_id)),
    h('td', { 'data-label': 'Release', class: 'mono small' }, d.version || '–'),
    h('td', { 'data-label': 'Trigger' }, d.trigger || '–'),
    h('td', { 'data-label': 'Stage' }, d.stage || '–'),
    h('td', { 'data-label': 'Started' }, timeEl(d.created_at)),
    h('td', { 'data-label': 'Duration', class: 'num' }, fmtDuration(durationBetween(d.created_at, d.completed_at))))));
  };
  const load = async () => {
    const [projects, list] = await Promise.all([api('GET', '/projects'), api('GET', '/deployments')]);
    state.projects = projects;
    deployments = list.reverse();
    if (!ctx.alive()) return;
    const selected = projectSel.value;
    replace(projectSel, h('option', { value: '' }, 'All apps'), projects.map((p) => h('option', { value: p.id, selected: p.id === selected }, p.name)));
    draw();
  };
  projectSel.addEventListener('change', draw);
  statusSel.addEventListener('change', draw);
  await load();
  const reload = debounce(() => load().catch(() => {}), 300);
  onEvents(ctx, (e) => {
    if (/^(Deployment|BuildStage|_resync)/.test(e.event_type)) reload();
  });
  every(ctx, 5000, () => {
    if (deployments.some(isActiveDeployment)) draw();
  });
  ctx.cleanup(reload.cancel);
}

async function deploymentPage(main, ctx, id) {
  const d = await api('GET', `/deployments/${enc(id)}`);
  if (!state.projects.length) state.projects = await api('GET', '/projects');
  if (!ctx.alive()) return;
  const name = projectName(d.project_id);
  const eventsList = h('ul', { class: 'events', 'data-events': '' });
  append(main,
    h('a', { class: 'small', href: '#/deployments' }, '← All deployments'),
    pageHeader(`Deployment ${shortId(d.id)}`, h('a', { class: 'btn', href: `#/apps/${enc(name)}` }, `Open ${name}`)),
    h('div', { class: 'grid' },
      pipelineCard(ctx, d.id),
      h('section', { class: 'card' }, h('div', { class: 'card-header' }, h('h2', {}, 'Events')), eventsList)));
  const loadEvents = async () => {
    const events = await api('GET', `/deployments/${enc(id)}/events`);
    if (ctx.alive()) replace(eventsList, events.map(eventItem));
  };
  await loadEvents();
  const reload = debounce(() => loadEvents().catch(() => {}), 200);
  onEvents(ctx, (e) => {
    if (e.event_type === '_resync' || e.payload?.deployment_id === id) reload();
  });
  ctx.cleanup(reload.cancel);
}

// ----------------------------------------------------------------------
// New app wizard
// ----------------------------------------------------------------------

async function wizardPage(main, ctx) {
  const body = h('div');
  append(main, h('a', { class: 'small', href: '#/' }, '← All apps'), pageHeader('New app'), body);

  // Step 1: where the code comes from.
  const kindGit = h('input', { type: 'radio', name: 'kind', value: 'git', checked: true });
  const kindDir = h('input', { type: 'radio', name: 'kind', value: 'dir' });
  const repo = h('input', { id: 'wiz-repo', class: 'mono', placeholder: 'https://github.com/you/app.git', autocomplete: 'off' });
  const branch = h('input', { id: 'wiz-branch', class: 'mono', value: 'main' });
  const dir = h('input', { id: 'wiz-dir', class: 'mono', placeholder: '/srv/apps/my-app' });
  const name = h('input', { id: 'wiz-name', placeholder: 'from the repository', autocomplete: 'off' });
  const gitFields = h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'wiz-repo' }, 'Repository URL'), repo, h('span', { class: 'hint' }, 'https, ssh or a path readable by the daemon')),
    h('div', { class: 'field' }, h('label', { for: 'wiz-branch' }, 'Branch'), branch));
  const dirFields = h('div', { class: 'field' }, h('label', { for: 'wiz-dir' }, 'Directory on this server'), dir);
  const syncKind = () => {
    gitFields.hidden = !kindGit.checked;
    dirFields.hidden = kindGit.checked;
  };
  kindGit.addEventListener('change', syncKind);
  kindDir.addEventListener('change', syncKind);
  syncKind();
  const error = h('div');
  const detectBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'detect' }, 'Detect settings →');

  const step1 = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      replace(error);
      const source = kindGit.checked
        ? { repository_url: repo.value.trim(), branch: branch.value.trim() || 'main' }
        : { source_dir: dir.value.trim() };
      if (!source.repository_url && !source.source_dir) {
        append(error, errorBox({ message: kindGit.checked ? 'Enter a repository URL' : 'Enter a directory' }));
        return;
      }
      detectBtn.textContent = 'Detecting…';
      try {
        detectBtn.disabled = true;
        const detected = await api('POST', '/detect', source);
        if (ctx.alive()) showStep2(source, detected);
      } catch (err) {
        append(error, errorBox(err));
      } finally {
        detectBtn.disabled = false;
        detectBtn.textContent = 'Detect settings →';
      }
    },
  },
  h('div', { class: 'card-header' }, h('h2', {}, '1. Source')),
  h('div', { class: 'card-body' },
    error,
    h('div', { class: 'field' },
      h('span', { class: 'sr-only' }, 'Source type'),
      h('div', { class: 'choice' },
        h('label', {}, kindGit, 'Git repository'),
        h('label', {}, kindDir, 'Directory on this server'))),
    gitFields,
    dirFields,
    h('div', { class: 'field' }, h('label', { for: 'wiz-name' }, 'App name'), name, h('span', { class: 'hint' }, 'Optional; used in URLs and the CLI')),
    h('div', { class: 'form-actions' }, detectBtn)));
  append(body, step1);

  const showStep2 = (source, detected) => {
    const appName = h('input', { id: 'wiz-app-name', required: true, value: name.value.trim() || detected.name });
    const { inputs, layout } = settingsInputs(detected);
    const deployNow = h('input', { type: 'checkbox', id: 'wiz-deploy', checked: true });
    const createBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'create' }, 'Create app');
    const err2 = h('div');
    const step2 = h('form', {
      class: 'card',
      onsubmit: async (e) => {
        e.preventDefault();
        replace(err2);
        createBtn.disabled = true;
        try {
          const created = await api('POST', '/projects', {
            name: appName.value.trim(),
            runtime: detected.runtime,
            ...source,
          });
          // Only what the operator changed becomes a dashboard setting;
          // everything else keeps following aegis.toml and detection.
          const overrides = {};
          for (const [key] of SETTING_FIELDS) {
            const v = inputs[key].value.trim();
            if (v !== (detected[key] || '')) overrides[key] = v;
          }
          const port = inputs.port.value ? Number(inputs.port.value) : null;
          if (port !== null && port !== detected.port) overrides.port = port;
          const health = inputs.health_check_url.value.trim();
          if (health !== (detected.health_check_url || '')) overrides.health_check_url = health;
          if (Object.keys(overrides).length) {
            await api('PUT', `/projects/${enc(created.id)}/settings`, overrides);
          }
          if (deployNow.checked) await api('POST', `/projects/${enc(created.id)}/deploy`, {});
          toast(deployNow.checked ? `${created.name} created; deploying` : `${created.name} created`);
          go(`/apps/${enc(created.name)}`);
        } catch (err) {
          if (ctx.alive()) append(err2, errorBox(err));
        } finally {
          createBtn.disabled = false;
        }
      },
    },
    h('div', { class: 'card-header' }, h('h2', {}, '2. Review'), h('span', { class: 'spacer' }), pill(`Detected: ${detected.runtime}`, 'info')),
    h('div', { class: 'card-body' },
      err2,
      h('div', { class: 'field' }, h('label', { for: 'wiz-app-name' }, 'App name'), appName),
      layout,
      h('label', { class: 'small', for: 'wiz-deploy' }, deployNow, ' Deploy right away'),
      h('div', { class: 'form-actions' },
        h('button', { class: 'btn', type: 'button', onclick: () => replace(body, step1) }, '← Back'),
        createBtn)));
    replace(body, step2);
    appName.focus();
  };
  repo.focus();
}

// ----------------------------------------------------------------------
// Account
// ----------------------------------------------------------------------

async function accountPage(main, ctx) {
  const session = await api('GET', '/auth/session');
  if (!ctx.alive()) return;

  // Password.
  const current = h('input', { id: 'pw-current', type: 'password', autocomplete: 'current-password', required: true });
  const next = h('input', { id: 'pw-new', type: 'password', autocomplete: 'new-password', required: true, minlength: 12 });
  const confirm = h('input', { id: 'pw-confirm', type: 'password', autocomplete: 'new-password', required: true, minlength: 12 });
  const pwBtn = h('button', { class: 'btn primary', type: 'submit' }, 'Change password');
  const pwForm = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      if (next.value !== confirm.value) {
        toast('The new passwords do not match', 'bad');
        return;
      }
      const ok = await run(pwBtn, () => api('POST', '/auth/password', { current_password: current.value, new_password: next.value }).then(() => true));
      if (ok) {
        toast('Password changed; sign in again');
        state.session = null;
        state.csrf = null;
        disconnectEvents();
        render();
      }
    },
  },
  h('div', { class: 'card-header' }, h('h2', {}, 'Password')),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, `Signed in as ${session.name}. Changing the password signs out every session.`),
    h('div', { class: 'field' }, h('label', { for: 'pw-current' }, 'Current password'), current),
    h('div', { class: 'field' }, h('label', { for: 'pw-new' }, 'New password'), next, h('span', { class: 'hint' }, 'At least 12 characters')),
    h('div', { class: 'field' }, h('label', { for: 'pw-confirm' }, 'Repeat new password'), confirm),
    h('div', { class: 'form-actions' }, pwBtn)));

  // API tokens.
  const tokenBody = h('tbody');
  const newToken = h('div');
  const tokenName = h('input', { id: 'token-name', required: true, placeholder: 'ci-deploy', autocomplete: 'off' });
  const tokenScope = h('select', { id: 'token-scope' },
    h('option', { value: 'deploy' }, 'deploy (read and act)'),
    h('option', { value: 'read' }, 'read (view only)'));
  const tokenBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'create-token' }, 'Create token');
  const loadTokens = async () => {
    const tokens = (await api('GET', '/tokens')).filter((t) => !t.revoked);
    if (!ctx.alive()) return;
    replace(tokenBody, tokens.length ? tokens.map((t) => h('tr', { 'data-token': t.name },
      h('td', { 'data-label': 'Name' }, t.name),
      h('td', { 'data-label': 'Scope' }, h('span', { class: 'tag' }, t.scope)),
      h('td', { 'data-label': 'Created' }, timeEl(t.created_at)),
      h('td', { 'data-label': 'Last used' }, t.last_used_at ? timeEl(t.last_used_at, fmtAgo(t.last_used_at)) : 'never'),
      h('td', { class: 'actions' }, h('button', {
        class: 'btn small danger',
        type: 'button',
        'data-action': 'revoke',
        onclick: async (e) => {
          const ok = await confirmDialog({
            title: `Revoke ${t.name}?`,
            message: 'Anything using this token loses access immediately.',
            confirmLabel: 'Revoke',
            danger: true,
          });
          if (!ok) return;
          await run(e.currentTarget, () => api('DELETE', `/tokens/${enc(t.id)}`), 'Token revoked');
          loadTokens().catch(() => {});
        },
      }, 'Revoke'))))
      : h('tr', {}, h('td', { colspan: 5, class: 'empty' }, 'No API tokens.')));
  };
  const tokenForm = h('form', {
    class: 'card-body',
    onsubmit: async (e) => {
      e.preventDefault();
      const res = await run(tokenBtn, () => api('POST', '/tokens', { name: tokenName.value.trim(), scope: tokenScope.value }));
      if (!res) return;
      tokenName.value = '';
      replace(newToken, h('div', { class: 'grid' },
        h('div', { class: 'alert warn' }, `Copy the token for ${res.info.name} now; it is not shown again.`),
        secretBox(res.token, 'API token')));
      loadTokens().catch(() => {});
    },
  },
  h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'token-name' }, 'Name'), tokenName),
    h('div', { class: 'field' }, h('label', { for: 'token-scope' }, 'Scope'), tokenScope)),
  h('div', { class: 'form-actions' }, tokenBtn),
  newToken);
  const tokenCard = h('section', { class: 'card' },
    h('div', { class: 'card-header' }, h('h2', {}, 'API tokens')),
    h('div', { class: 'card-body' }, h('p', { class: 'muted small' }, 'For the CLI and CI: send as ', h('code', {}, 'Authorization: Bearer <token>'), '.')),
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Name', 'Scope', 'Created', 'Last used', ''].map((t) => h('th', {}, t)))),
      tokenBody)),
    tokenForm);

  // Appearance.
  const themeSel = h('select', { id: 'theme', 'aria-label': 'Theme' },
    [['system', 'Match the system'], ['light', 'Light'], ['dark', 'Dark']].map(([v, l]) => h('option', { value: v, selected: storedTheme() === v }, l)));
  themeSel.addEventListener('change', () => applyTheme(themeSel.value));
  const prefs = h('section', { class: 'card' },
    h('div', { class: 'card-header' }, h('h2', {}, 'Appearance & shortcuts')),
    h('div', { class: 'card-body' },
      h('div', { class: 'field' }, h('label', { for: 'theme' }, 'Theme'), themeSel),
      shortcutList()));

  append(main, pageHeader('Account', h('button', { class: 'btn', type: 'button', onclick: signOut }, 'Sign out')),
    h('div', { class: 'grid' }, tokenCard, h('div', { class: 'grid two' }, pwForm, prefs)));
  await loadTokens();
}

// ----------------------------------------------------------------------
// Keyboard shortcuts
// ----------------------------------------------------------------------

const SHORTCUTS = [
  ['g o', 'Overview'],
  ['g d', 'Deployments'],
  ['g e', 'Events'],
  ['g a', 'Account'],
  ['n', 'New app'],
  ['1 – 5', 'Switch tabs on an app page'],
  ['/', 'Search (logs and events)'],
  ['t', 'Toggle light/dark theme'],
  ['?', 'Show shortcuts'],
];

function shortcutList() {
  return h('div', { class: 'shortcuts' },
    SHORTCUTS.map(([keys, what]) => [h('span', {}, keys.split(' ').map((k) => (k === '–' ? ' – ' : h('kbd', {}, k)))), h('span', {}, what)]));
}

let pendingG = null;
document.addEventListener('keydown', (e) => {
  if (!state.session || e.ctrlKey || e.metaKey || e.altKey) return;
  const t = e.target;
  if (t instanceof HTMLElement && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName))) return;
  if (document.querySelector('dialog[open]')) return;
  if (pendingG) {
    clearTimeout(pendingG);
    pendingG = null;
    const dest = { o: '/', d: '/deployments', e: '/events', a: '/account' }[e.key];
    if (dest) {
      e.preventDefault();
      go(dest);
    }
    return;
  }
  switch (e.key) {
    case 'g':
      pendingG = setTimeout(() => {
        pendingG = null;
      }, 1200);
      break;
    case 'n':
      go('/apps/new');
      break;
    case 't':
      toggleTheme();
      break;
    case '/': {
      const s = $('[data-search]');
      if (s) {
        e.preventDefault();
        s.focus();
      }
      break;
    }
    case '?':
      modal(() => h('form', { method: 'dialog' },
        h('div', { class: 'dialog-body' }, h('h2', {}, 'Keyboard shortcuts'), shortcutList()),
        h('div', { class: 'form-actions' }, h('button', { class: 'btn primary', value: 'ok', autofocus: true }, 'Close'))));
      break;
    default: {
      const m = currentPath().match(/^\/apps\/([^/]+)/);
      const i = Number(e.key) - 1;
      if (m && m[1] !== 'new' && i >= 0 && i < APP_TABS.length) go(`/apps/${m[1]}/${APP_TABS[i][0]}`);
    }
  }
});

// ----------------------------------------------------------------------
// Boot
// ----------------------------------------------------------------------

window.addEventListener('hashchange', render);

(async () => {
  try {
    startSession(await api('GET', '/auth/session'));
  } catch {
    state.session = null;
    render();
  }
})();
