// Aegis dashboard: a small single-page app with no build step and no
// dependencies. It talks to the same /api/v1 endpoints as the CLI, signed in
// with the session cookie. The page's content security policy forbids inline
// scripts and styles, so everything here builds DOM nodes directly (never
// innerHTML with data) and sets styles only through the CSSOM.

'use strict';

import { HELP, TOUR, DOCS_BASE } from './help.js';

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

const ROLE_RANK = { viewer: 0, deployer: 1, admin: 2 };

/** Whether the signed-in user has at least `role`. Only hides controls; the
 * server enforces roles on every request. */
function can(role) {
  return (ROLE_RANK[state.session?.role] ?? -1) >= ROLE_RANK[role];
}

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
  window.dispatchEvent(new Event('aegis-theme'));
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
  'ResourceLimitsChanged', 'ResourcePressureDetected',
  'DomainMapped', 'DomainUnmapped',
  'BackupCreated', 'BackupFailed', 'BackupDeleted',
  'UpdateAvailable', 'UpdateStarted', 'UpdateApplied', 'UpdateFailed',
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
// Aegis updates: banner, "Upgrade now", reconnecting after the restart
// ----------------------------------------------------------------------

const update = { status: null, timer: null };
const UPDATE_PHASES = {
  downloading: 'Downloading and verifying',
  backing_up: 'Backing up',
  installing: 'Installing',
  waiting_for_helper: 'Waiting for the update helper',
  restarting: 'Restarting',
};

function dismissed(key) {
  try {
    return localStorage.getItem(`aegis-dismissed-${key}`) === '1';
  } catch {
    return false;
  }
}

function dismiss(key) {
  try {
    localStorage.setItem(`aegis-dismissed-${key}`, '1');
  } catch {
    /* storage unavailable: hide until the next reload */
  }
  update.hidden = key;
  renderUpdateBanner();
}

async function refreshUpdate() {
  if (!state.session) return;
  clearTimeout(update.timer);
  try {
    update.status = await api('GET', '/system/update');
  } catch {
    update.timer = setTimeout(refreshUpdate, 60 * 1000);
    return;
  }
  renderUpdateBanner();
  if (UPDATE_PHASES[update.status.phase]) waitForNewVersion(update.status.current_version);
  else update.timer = setTimeout(refreshUpdate, 10 * 60 * 1000);
}

function renderUpdateBanner() {
  const el = $('#update-banner');
  const s = update.status;
  if (!el || !s) return;
  const hidden = (key) => update.hidden === key || dismissed(key);
  const admin = can('admin');
  const latest = s.latest;
  const busy = UPDATE_PHASES[s.phase];
  if (busy) {
    replace(el, h('div', { class: 'alert info', 'data-update': 'progress' },
      h('strong', {}, `Updating Aegis to ${latest?.version || 'a new version'}… `),
      `${busy}. `,
      s.phase === 'restarting' || s.phase === 'waiting_for_helper'
        ? 'Aegis restarts in a moment; this page reconnects by itself.'
        : null));
    return;
  }
  const failedKey = `failed-${s.phase_error}`;
  if (s.phase === 'failed' && admin && !hidden(failedKey)) {
    replace(el, h('div', { class: 'alert', role: 'alert', 'data-update': 'failed' },
      h('strong', {}, 'The update failed. '), s.phase_error || '', ' ',
      h('button', { class: 'btn small', type: 'button', onclick: () => dismiss(failedKey) }, 'Dismiss')));
    return;
  }
  const updatedKey = `updated-${s.current_version}`;
  const recent = s.last_update && s.last_update.to_version === s.current_version
    && Date.now() - parseTime(s.last_update.at) < 24 * 3600 * 1000;
  if (recent && !hidden(updatedKey)) {
    replace(el, h('div', { class: 'alert ok', 'data-update': 'updated' },
      h('strong', {}, `Updated to v${s.current_version}`), ` (from v${s.last_update.from_version.replace(/^v/, '')}). `,
      s.last_update.signed_by ? `Signed by release key ${s.last_update.signed_by}. `
        : s.last_update.allow_unsigned ? 'Installed without a signature check (--allow-unsigned). ' : null,
      h('button', { class: 'btn small', type: 'button', onclick: () => dismiss(updatedKey) }, 'Dismiss')));
    return;
  }
  const availableKey = `available-${latest?.version}`;
  if (!s.update_available || !latest || hidden(availableKey)) {
    replace(el);
    return;
  }
  const parts = [
    h('strong', {}, `Aegis ${latest.version} is available`),
    ` (you run v${s.current_version}). `,
    latest.url ? h('a', { href: latest.url, target: '_blank', rel: 'noopener noreferrer' }, 'Release notes') : null,
    ' ',
  ];
  if (admin) {
    if (s.can_apply) {
      parts.push(h('button', {
        class: 'btn small primary',
        type: 'button',
        'data-action': 'upgrade',
        onclick: (e) => upgradeNow(e.currentTarget),
      }, 'Upgrade now'));
    } else if (s.blocked_reason) {
      parts.push(h('div', { class: 'small' }, s.blocked_reason));
    } else if (s.mode === 'manual' && s.manual_command) {
      parts.push(
        h('div', { class: 'small' }, s.mode_detail, ' Back up first (Account → Backups), then run on the server:'),
        h('pre', { class: 'restore-steps' }, h('code', {}, s.manual_command)),
        copyButton(s.manual_command));
    } else if (!s.check_enabled || s.mode === 'unsupported') {
      parts.push(h('div', { class: 'small' }, s.mode_detail));
    }
  }
  parts.push(' ', h('button', { class: 'btn small ghost', type: 'button', onclick: () => dismiss(availableKey) }, 'Later'), ' ', helpButton('update'));
  replace(el, h('div', { class: 'alert info', 'data-update': 'available' }, parts));
}

async function upgradeNow(btn) {
  const s = update.status;
  const version = s.latest.version;
  const how = s.mode === 'helper'
    ? 'The root-owned aegis-update helper service installs it (Aegis itself can\'t write its binary).'
    : 'Aegis replaces its own binary.';
  const answer = await modal(() => h('form', { method: 'dialog' },
    h('div', { class: 'dialog-body' },
      h('h2', {}, `Upgrade Aegis to ${version}?`),
      h('p', { class: 'muted' }, `You run v${s.current_version}. ${how}`),
      h('ol', { class: 'update-steps' },
        h('li', {}, 'A backup of the database, secrets key and certificates is taken first (Account → Backups; release directories are not included).'),
        h('li', {}, `${version} is downloaded from GitHub. Its SHA-256 checksums must be signed by the Aegis release key (${(s.release_keys || []).join(', ')}) and the download must match them. If anything fails before the swap, nothing changes.`),
        h('li', {}, 'The Aegis binaries are replaced; the old daemon is kept next to the new one as aegis-daemon.previous.'),
        h('li', {}, 'Aegis restarts. Your apps are stopped during the restart and started again automatically, so expect a short outage (usually a few seconds). Deployments in progress are marked failed. The dashboard reconnects by itself.')),
      h('p', { class: 'muted small' }, 'Going back later may need the pre-update backup if the new version upgraded the database. See docs/upgrading.md.')),
    h('div', { class: 'form-actions' },
      h('button', { class: 'btn', value: 'cancel', type: 'submit' }, 'Cancel'),
      h('button', { class: 'btn primary', value: 'ok', type: 'submit', autofocus: true, 'data-confirm': '' }, `Upgrade to ${version}`))));
  if (answer !== 'ok') return;
  const res = await run(btn, () => api('POST', '/system/update/apply', { version }));
  if (!res) return;
  update.status = res;
  renderUpdateBanner();
  waitForNewVersion(s.current_version);
}

/** Polls until a daemon answers with another version (then loads its
 * dashboard) or the update fails. */
function waitForNewVersion(before) {
  clearTimeout(update.timer);
  const poll = async () => {
    let s;
    try {
      const res = await fetch('/api/v1/system/update', { credentials: 'same-origin', headers: { Accept: 'application/json' } });
      if (!res.ok) throw new Error(String(res.status));
      s = await res.json();
    } catch {
      const el = $('#update-banner');
      if (el) replace(el, h('div', { class: 'alert info', 'data-update': 'progress' }, h('strong', {}, 'Aegis is restarting… '), 'reconnecting.'));
      update.timer = setTimeout(poll, 1500);
      return;
    }
    update.status = s;
    if (s.current_version !== before) {
      location.reload();
      return;
    }
    renderUpdateBanner();
    if (s.phase === 'failed') {
      toast(`Update failed: ${s.phase_error || 'see Events'}`, 'bad');
      update.timer = setTimeout(refreshUpdate, 10 * 60 * 1000);
      return;
    }
    update.timer = setTimeout(poll, 2000);
  };
  update.timer = setTimeout(poll, 1000);
}

// Update events (a check found a release, another admin started an update...).
state.eventListeners.add((e) => {
  if (e.event_type.startsWith('Update')) refreshUpdate();
});

// ----------------------------------------------------------------------
// Router
// ----------------------------------------------------------------------

const ROUTES = [
  [/^\/?$/, overviewPage, 'overview'],
  [/^\/apps\/new$/, wizardPage, 'overview'],
  [/^\/apps\/([^/]+)(?:\/([a-z]+))?$/, appPage, 'overview'],
  [/^\/groups\/new$/, newGroupPage, 'overview'],
  [/^\/groups\/([^/]+)$/, groupPage, 'overview'],
  [/^\/deployments$/, deploymentsPage, 'deployments'],
  [/^\/deployments\/([^/]+)$/, deploymentPage, 'deployments'],
  [/^\/events$/, eventsPage, 'events'],
  [/^\/account$/, accountPage, 'account'],
];

function currentPath() {
  // A query (e.g. ?process=<id> on the logs tab) is read by the page itself.
  return decodeURI(location.hash.replace(/^#/, '').split('?')[0]) || '/';
}

function go(path) {
  location.hash = `#${path}`;
}

function teardownPage() {
  state.routeToken += 1;
  closeHelp(false);
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
    const banner = h('div', { id: 'update-banner', class: 'update-banner', 'aria-live': 'polite' });
    const bar = h('header', { class: 'topbar' },
      h('a', { class: 'brand', href: '#/' }, h('img', { src: '/assets/favicon.svg', alt: '' }), h('span', {}, 'Aegis')),
      nav,
      h('div', { class: 'topbar-actions' },
        h('span', { id: 'conn', class: 'pill warn live' }, 'Connecting'),
        h('button', {
          class: 'btn ghost icon', type: 'button', title: 'Help and shortcuts (?)', 'aria-label': 'Help and keyboard shortcuts',
          'data-tour': 'help', onclick: showHelpDialog,
        }, '?'),
        h('button', { class: 'btn ghost icon', type: 'button', title: 'Toggle theme (t)', 'aria-label': 'Toggle theme', 'data-tour': 'theme', onclick: toggleTheme }, '◐'),
        h('button', { class: 'btn ghost', type: 'button', id: 'signout', onclick: signOut }, 'Sign out')));
    state.shell = { root: h('div', {}, bar, banner, main), main };
    replace($('#app'), state.shell.root);
    refreshUpdate();
  }
  for (const a of state.shell.root.querySelectorAll('[data-nav]')) {
    if (a.dataset.nav === section) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  replace(state.shell.main);
  window.scrollTo(0, 0);
  return state.shell.main;
}

/** A page title with its help button (`help` is a key of HELP). */
function pageHeader(title, help, ...right) {
  return h('div', { class: 'page-header' }, h('h1', {}, title), helpButton(help), h('div', { class: 'spacer' }), right);
}

/** A card header: its title, the help button, then anything else. */
function cardHeader(title, help, ...rest) {
  return h('div', { class: 'card-header' }, h('h2', {}, title), helpButton(help), rest);
}

// ----------------------------------------------------------------------
// Section help: a "?" button next to a heading opens a popover explaining
// the section (text in help.js, mirrored by docs/dashboard/).
// ----------------------------------------------------------------------

const helpPop = { el: null, btn: null };

function helpButton(key) {
  const entry = HELP[key];
  if (!entry) return null;
  return h('button', {
    class: 'help-btn',
    type: 'button',
    'aria-label': 'Help',
    title: `Help: ${entry.title}`,
    'aria-expanded': 'false',
    'aria-haspopup': 'dialog',
    'data-help': key,
    onclick: (e) => {
      const btn = e.currentTarget;
      if (helpPop.btn === btn) closeHelp();
      else openHelp(btn, key);
    },
  }, '?');
}

function closeHelp(restoreFocus = true) {
  const { el, btn } = helpPop;
  helpPop.el = null;
  helpPop.btn = null;
  if (el) el.remove();
  if (btn) {
    btn.setAttribute('aria-expanded', 'false');
    btn.removeAttribute('aria-controls');
    if (restoreFocus && btn.isConnected) btn.focus();
  }
}

function placeHelp() {
  const { el, btn } = helpPop;
  if (!el || !btn) return;
  if (!btn.isConnected) {
    closeHelp(false);
    return;
  }
  const narrow = window.innerWidth < 600;
  el.classList.toggle('sheet', narrow);
  if (narrow) {
    el.style.removeProperty('top');
    el.style.removeProperty('left');
    return;
  }
  const r = btn.getBoundingClientRect();
  const width = el.offsetWidth;
  const height = el.offsetHeight;
  const docWidth = document.documentElement.clientWidth;
  const left = Math.max(8, Math.min(r.left - 12, docWidth - width - 8));
  const below = r.bottom + 8;
  const top = below + height > window.innerHeight && r.top - height - 8 > 0 ? r.top - height - 8 : below;
  el.style.setProperty('left', `${left + window.scrollX}px`);
  el.style.setProperty('top', `${top + window.scrollY}px`);
}

function openHelp(btn, key) {
  closeHelp(false);
  const entry = HELP[key];
  const el = h('div', {
    class: 'help-pop',
    id: 'help-pop',
    role: 'dialog',
    'aria-labelledby': 'help-pop-title',
    tabindex: '-1',
    'data-help-pop': key,
  },
  h('div', { class: 'help-pop-head' },
    h('h2', { id: 'help-pop-title' }, entry.title),
    h('button', { class: 'btn ghost icon small', type: 'button', 'aria-label': 'Close help', onclick: () => closeHelp() }, '✕')),
  h('p', {}, entry.summary),
  entry.controls.length
    ? h('dl', { class: 'help-controls' }, entry.controls.map(([control, what]) => h('div', {}, h('dt', {}, control), h('dd', {}, what))))
    : null,
  h('p', { class: 'help-roles small' }, h('strong', {}, 'Who can use it: '), entry.roles),
  h('a', { class: 'small', href: `${DOCS_BASE}${entry.docs}`, target: '_blank', rel: 'noopener noreferrer', 'data-help-docs': '' }, 'Read more in the dashboard guide ↗'));
  document.body.append(el);
  helpPop.el = el;
  helpPop.btn = btn;
  btn.setAttribute('aria-expanded', 'true');
  btn.setAttribute('aria-controls', 'help-pop');
  placeHelp();
  el.focus();
}

document.addEventListener('pointerdown', (e) => {
  if (!helpPop.el) return;
  if (helpPop.el.contains(e.target) || helpPop.btn.contains(e.target)) return;
  closeHelp(false);
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && helpPop.el && !document.querySelector('dialog[open]')) {
    e.preventDefault();
    closeHelp();
  }
});
window.addEventListener('resize', placeHelp);

/** The "?" overlay: shortcuts, the guide and the tour. */
async function showHelpDialog() {
  const answer = await modal(() => h('form', { method: 'dialog' },
    h('div', { class: 'dialog-body' },
      h('h2', {}, 'Help and keyboard shortcuts'),
      h('p', { class: 'muted small' },
        'Every page and card has a ', h('span', { class: 'help-btn static', 'aria-hidden': 'true' }, '?'),
        ' button that explains what it shows and who can use it. The ',
        h('a', { href: `${DOCS_BASE}README.md`, target: '_blank', rel: 'noopener noreferrer' }, 'dashboard guide'),
        ' covers everything step by step.'),
      h('h3', { class: 'dialog-sub' }, 'Keyboard shortcuts'),
      shortcutList()),
    h('div', { class: 'form-actions' },
      h('button', { class: 'btn', value: 'tour', 'data-action': 'start-tour' }, 'Take the tour'),
      h('span', { class: 'spacer' }),
      h('button', { class: 'btn primary', value: 'ok', autofocus: true }, 'Close'))));
  if (answer === 'tour') startTour();
}

// ----------------------------------------------------------------------
// First-run tour (steps in help.js). Shown once per user; completion is
// remembered in this browser, keyed by username.
// ----------------------------------------------------------------------

const tour = { active: null, doneHere: new Set() };

function tourKey() {
  return `aegis.tour.${state.session?.name || ''}`;
}

function tourDone() {
  if (tour.doneHere.has(tourKey())) return true;
  try {
    return localStorage.getItem(tourKey()) === 'done';
  } catch {
    return false;
  }
}

function markTourDone() {
  tour.doneHere.add(tourKey());
  try {
    localStorage.setItem(tourKey(), 'done');
  } catch {
    /* storage unavailable: done for this page only */
  }
}

const reducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function visibleEl(selector) {
  for (const el of document.querySelectorAll(selector)) {
    if (el.getClientRects().length) return el;
  }
  return null;
}

async function waitForEl(selector, ms) {
  const until = Date.now() + ms;
  for (;;) {
    const el = visibleEl(selector);
    if (el || Date.now() >= until) return el;
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function startTour() {
  if (tour.active || !state.session) return;
  closeHelp(false);
  let apps = state.projects;
  try {
    apps = await api('GET', '/projects');
  } catch {
    /* use what we have */
  }
  if (tour.active || !state.session) return;
  const steps = TOUR.filter((s) => (!s.role || can(s.role)) && (!s.needsApp || apps.length > 0));
  const counter = h('div', { class: 'tour-count small muted', 'aria-live': 'polite' });
  const title = h('h2', { id: 'tour-title' });
  const text = h('p', { id: 'tour-text' });
  const back = h('button', { class: 'btn', type: 'button', 'data-tour-action': 'back' }, 'Back');
  const next = h('button', { class: 'btn primary', type: 'button', 'data-tour-action': 'next' }, 'Next');
  const skip = h('button', { class: 'btn ghost', type: 'button', 'data-tour-action': 'skip' }, 'Skip tour');
  const card = h('div', {
    class: 'tour-card', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'tour-title', 'aria-describedby': 'tour-text', tabindex: '-1',
  }, counter, title, text, h('div', { class: 'tour-actions' }, skip, h('span', { class: 'spacer' }), back, next));
  const spot = h('div', { class: 'tour-spot', 'aria-hidden': 'true' });
  const overlay = h('div', { class: 'tour', 'data-tour-overlay': '' }, spot, card);
  const t = {
    steps, index: 0, overlay, spot, card, counter, title, text, back, next,
    target: null, token: 0, appPath: apps[0] ? `/apps/${enc(apps[0].name)}` : null,
    returnTo: location.hash || '#/', opener: document.activeElement,
  };
  tour.active = t;
  back.addEventListener('click', () => showStep(t.index - 1, -1));
  next.addEventListener('click', () => showStep(t.index + 1, 1));
  skip.addEventListener('click', () => endTour(true));
  t.onKey = (e) => {
    if (tour.active !== t) return;
    if (document.querySelector('dialog[open]')) return;
    const onButton = e.target instanceof HTMLButtonElement && card.contains(e.target);
    if (e.key === 'Escape') {
      e.preventDefault();
      endTour(true);
    } else if (e.key === 'ArrowRight' || (e.key === 'Enter' && !onButton)) {
      e.preventDefault();
      showStep(t.index + 1, 1);
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault();
      if (t.index > 0) showStep(t.index - 1, -1);
    } else if (e.key === 'Tab') {
      // Keep focus in the step card.
      const focusable = [...card.querySelectorAll('button')].filter((b) => !b.disabled && b.getClientRects().length);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!card.contains(document.activeElement)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };
  t.onMove = () => placeTour();
  document.addEventListener('keydown', t.onKey, true);
  window.addEventListener('resize', t.onMove);
  window.addEventListener('scroll', t.onMove, true);
  // Live pages redraw; keep the highlight on the current element.
  t.timer = setInterval(placeTour, 300);
  document.body.append(overlay);
  showStep(0, 1);
}

function stepRoute(t, step) {
  if (step.route === 'app') {
    if (/^\/apps\/(?!new$)[^/]+/.test(currentPath())) return undefined;
    return t.appPath;
  }
  return step.route;
}

async function showStep(i, dir) {
  const t = tour.active;
  if (!t) return;
  t.token += 1;
  const token = t.token;
  if (i < 0) return;
  if (i >= t.steps.length) {
    endTour(true);
    return;
  }
  const step = t.steps[i];
  let target = null;
  if (step.target) {
    const path = stepRoute(t, step);
    if (path === null) {
      showStep(i + dir, dir);
      return;
    }
    if (path !== undefined && currentPath() !== path) go(path);
    target = await waitForEl(step.target, path !== undefined ? 5000 : 1000);
    if (tour.active !== t || token !== t.token) return;
    if (!target) {
      // Not on this page (or hidden for this role): skip it.
      if (i + dir < 0) return;
      showStep(i + dir, dir);
      return;
    }
    target.scrollIntoView({ block: 'center', inline: 'nearest', behavior: reducedMotion() ? 'auto' : 'smooth' });
  }
  t.index = i;
  t.step = step;
  t.target = target;
  t.overlay.classList.toggle('no-target', !target);
  t.overlay.dataset.step = step.id;
  t.counter.textContent = `Step ${i + 1} of ${t.steps.length}`;
  t.title.textContent = step.title;
  t.text.textContent = step.text;
  t.back.disabled = i === 0;
  t.next.textContent = i === t.steps.length - 1 ? 'Finish' : 'Next';
  placeTour();
  t.next.focus({ preventScroll: true });
}

function placeTour() {
  const t = tour.active;
  if (!t) return;
  let target = t.target;
  if (target && !target.isConnected && t.step?.target) {
    target = visibleEl(t.step.target);
    t.target = target;
  }
  const vw = document.documentElement.clientWidth;
  const vh = window.innerHeight;
  const card = t.card;
  const cw = card.offsetWidth;
  const ch = card.offsetHeight;
  const set = (el, props) => {
    for (const [k, v] of Object.entries(props)) {
      if (v === null) el.style.removeProperty(k);
      else el.style.setProperty(k, `${Math.round(v)}px`);
    }
  };
  if (!target) {
    t.spot.hidden = true;
    set(card, { left: (vw - cw) / 2, top: Math.max(12, (vh - ch) / 2) });
    return;
  }
  t.spot.hidden = false;
  const r = target.getBoundingClientRect();
  const pad = 6;
  set(t.spot, { left: r.left - pad, top: r.top - pad, width: r.width + pad * 2, height: r.height + pad * 2 });
  if (vw < 600) {
    // Phones: dock the card away from the highlighted element.
    const top = r.top + r.height / 2 > vh / 2 ? 12 : vh - ch - 12;
    set(card, { left: (vw - cw) / 2, top });
    return;
  }
  let top = r.bottom + pad + 12;
  if (top + ch > vh - 12) top = r.top - pad - 12 - ch;
  if (top < 12) top = Math.min(vh - ch - 12, Math.max(12, r.bottom + pad + 12));
  const left = Math.max(12, Math.min(r.left + r.width / 2 - cw / 2, vw - cw - 12));
  set(card, { left, top });
}

/** Ends the tour; `done` remembers it so it is not shown again. */
function endTour(done) {
  const t = tour.active;
  if (!t) return;
  tour.active = null;
  if (done) markTourDone();
  clearInterval(t.timer);
  document.removeEventListener('keydown', t.onKey, true);
  window.removeEventListener('resize', t.onMove);
  window.removeEventListener('scroll', t.onMove, true);
  t.overlay.remove();
  if (!state.session) return;
  if ((location.hash || '#/') !== t.returnTo) location.hash = t.returnTo;
  const back = t.opener instanceof HTMLElement && t.opener.isConnected && t.opener !== document.body ? t.opener : null;
  if (back) back.focus();
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
  document.body.dataset.role = session.role || 'viewer';
  state.csrf = session.csrf_token;
  state.shell = null;
  connectEvents();
  render().then(() => {
    // First visit for this user: show the tour once.
    if (state.session === session && session.kind !== 'token' && !tourDone()) startTour();
  });
}

function loginPage() {
  endTour(false);
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
    pageHeader('Overview', 'overview',
      h('button', { class: 'btn', type: 'button', 'data-action': 'unlock-app', onclick: () => unlockAppDialog(() => loadApps().catch(() => {})) }, 'Unlock hidden app'),
      can('admin') ? h('a', { class: 'btn', href: '#/groups/new', 'data-action': 'new-group' }, '+ New group') : null,
      h('a', { class: 'btn primary', href: '#/apps/new', 'data-requires': 'admin', 'data-tour': 'new-app' }, '+ New app')),
    h('div', { class: 'section-head' }, h('h2', {}, 'Server'), helpButton('overview.host')),
    hostEl,
    h('div', { class: 'section' }, h('div', { class: 'section-head' }, h('h2', {}, 'Apps'), helpButton('overview.apps')), appsEl));

  const loadHost = async () => {
    try {
      const host = await api('GET', '/host');
      if (ctx.alive()) replace(hostEl, hostTiles(host));
    } catch (err) {
      if (ctx.alive()) replace(hostEl, errorBox(err));
    }
  };
  const loadApps = async () => {
    const [projects, deployments, groups] = await Promise.all([api('GET', '/projects'), api('GET', '/deployments'), api('GET', '/groups')]);
    state.projects = projects;
    if (!ctx.alive()) return;
    if (projects.length && groups.length) {
      const active = new Map();
      for (const d of deployments) if (isActiveDeployment(d)) active.set(d.project_id, d);
      const grouped = new Set(groups.flatMap((g) => g.members.map((m) => m.project_id)));
      const byId = new Map(projects.map((p) => [p.id, p]));
      replace(appsEl,
        h('div', { class: 'grid' }, groups.map((g) => groupCard(g, byId, active, loadApps))),
        h('div', { class: 'grid apps' }, projects.filter((p) => !grouped.has(p.id)).map((p) => appCard(p, active.get(p.id), loadApps))));
      return;
    }
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
          : (project.instances || []).length > 1
            ? 'Its instances restart one at a time; the others keep serving.'
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

/** "3 new commits", "Uncommitted changes" or a failed check; nothing when up to date. */
function updatesBadge(u, { relevantOnly = false } = {}) {
  if (!u) return null;
  if (u.error && u.behind == null) return h('span', { 'data-updates': 'error', title: u.error }, pill('Check failed', 'warn'));
  const bits = [];
  if (u.behind > 0 && (!relevantOnly || u.relevant)) {
    bits.push(h('span', { 'data-updates': String(u.behind), title: `Checked ${fmtAgo(u.checked_at)}` },
      pill(`${u.behind} new commit${u.behind === 1 ? '' : 's'}`, 'info')));
  }
  if (u.dirty > 0) bits.push(h('span', { 'data-updates': 'dirty' }, pill('Uncommitted changes', 'warn')));
  return bits.length ? bits : null;
}

function appCard(project, activeDeployment, reload) {
  const p = project.process;
  return h('article', { class: 'card app-card', 'data-app': project.name },
    h('div', { class: 'card-body' },
      h('div', { class: 'title' },
        h('a', { href: `#/apps/${enc(project.name)}` }, project.name),
        project.protected ? h('span', { class: 'pill info', title: 'Hidden behind a password; unlocked for this session', 'data-hidden-app': '' }, 'Hidden') : null,
        h('span', { class: 'spacer' }),
        updatesBadge(project.updates),
        activeDeployment ? deploymentPill(activeDeployment.status) : null,
        processPill(project)),
      h('div', { class: 'source muted truncate', title: sourceText(project) }, sourceText(project)),
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Version'), h('dd', { class: 'truncate', title: project.current_release || '' }, project.current_release || '–')),
        h('div', {}, h('dt', {}, 'Uptime'), h('dd', {}, uptime(p))),
        h('div', {}, h('dt', {}, 'Restarts'), h('dd', {}, p ? p.restart_count : '–')),
        h('div', { 'data-fact': 'cpu' }, h('dt', {}, 'CPU'), h('dd', {}, project.usage ? fmtCores(project.usage.cpu_cores, project.resources.cpu_cores) : '–')),
        h('div', { 'data-fact': 'memory' }, h('dt', {}, 'Memory'), h('dd', {}, project.usage ? fmtUsage(project.usage.memory_bytes, project.resources.memory_bytes) : '–')))),
    h('div', { class: 'actions' },
      h('button', { class: 'btn small primary', type: 'button', 'data-action': 'deploy', onclick: (e) => quickDeploy(project, e.currentTarget) }, 'Deploy'),
      processButtons(project, reload),
      h('a', { class: 'btn small ghost', href: `#/apps/${enc(project.name)}/logs` }, 'Logs')));
}

// ----------------------------------------------------------------------
// Groups
// ----------------------------------------------------------------------

const GROUP_RUN_KIND = { Running: 'info', Success: 'ok', Failed: 'bad', RolledBack: 'warn' };
const MEMBER_RUN_KIND = { deployed: 'ok', skipped: '', failed: 'bad', rolled_back: 'warn', deploying: 'info', pending: '' };

/** Deploy all, deploy the changed apps only, or roll back a group. */
function groupButtons(group, after, { small = true } = {}) {
  const cls = small ? 'btn small' : 'btn';
  if (!can('deployer')) return [];
  const deploy = (onlyChanged) => async (e) => {
    const btn = e.currentTarget;
    const res = await run(btn, () => api('POST', `/groups/${enc(group.id)}/deploy`, { only_changed: onlyChanged }),
      onlyChanged ? `Deploying the changed apps of ${group.name}` : `Deploying ${group.name}`);
    if (res && after) after();
  };
  const rollback = async (e) => {
    const btn = e.currentTarget;
    const ok = await confirmDialog({
      title: `Roll back ${group.name}?`,
      message: 'Every app the latest group deployment switched goes back to the release it had before.',
      confirmLabel: 'Roll back group',
    });
    if (!ok) return;
    const res = await run(btn, () => api('POST', `/groups/${enc(group.id)}/rollback`),
      (r) => `Rolled back ${r.members.length} app${r.members.length === 1 ? '' : 's'}`);
    if (res && after) after();
  };
  const busy = group.last_deployment?.status === 'Running';
  return [
    h('button', { class: `${cls} primary`, type: 'button', 'data-action': 'deploy-group', disabled: busy, onclick: deploy(false) }, 'Deploy group'),
    h('button', { class: cls, type: 'button', 'data-action': 'deploy-changed', disabled: busy, onclick: deploy(true) }, 'Deploy changed only'),
    h('button', { class: cls, type: 'button', 'data-action': 'rollback-group', disabled: busy, onclick: rollback }, 'Roll back group'),
  ];
}

function groupRunPill(run) {
  if (!run) return null;
  return h('span', { 'data-group-run': run.status }, pill(run.status === 'Running' ? 'Deploying' : run.status, GROUP_RUN_KIND[run.status] ?? ''));
}

/** Overview: a group as one card with its apps inside. */
function groupCard(group, projects, active, reload) {
  const runOf = (id) => group.last_deployment?.members.find((m) => m.project_id === id);
  const rows = group.members.map((m) => {
    const p = projects.get(m.project_id);
    const run = runOf(m.project_id);
    return h('tr', { 'data-member': m.name },
      h('td', {}, h('a', { href: `#/apps/${enc(m.name)}` }, m.name)),
      h('td', {}, p ? processPill(p) : null, active.get(m.project_id) ? deploymentPill(active.get(m.project_id).status) : null),
      h('td', { class: 'mono small truncate', title: m.current_release || '' }, m.current_release || '–'),
      h('td', { class: 'small', 'data-member-updates': '' },
        run && group.last_deployment.status === 'Running'
          ? pill(run.status, MEMBER_RUN_KIND[run.status] ?? '')
          : (p?.updates?.relevant && !p.updates.up_to_date && p.updates.behind > 0
            ? h('span', { 'data-changed': '' }, pill('Changed since live', 'info'))
            : null)));
  });
  const changed = group.members.filter((m) => {
    const u = projects.get(m.project_id)?.updates;
    return u && u.relevant && !u.up_to_date && u.behind > 0;
  }).length;
  const behind = Math.max(0, ...group.members.map((m) => projects.get(m.project_id)?.updates?.behind ?? 0));
  return h('article', { class: 'card group-card', 'data-group': group.name },
    h('div', { class: 'card-body' },
      h('div', { class: 'title' },
        h('a', { href: `#/groups/${enc(group.name)}` }, group.name),
        h('span', { class: 'muted small' }, ` · ${group.members.length} app${group.members.length === 1 ? '' : 's'}`),
        h('span', { class: 'spacer' }),
        h('span', { 'data-group-updates': String(changed) },
          behind > 0 ? pill(`${behind} new commit${behind === 1 ? '' : 's'} · ${changed} app${changed === 1 ? '' : 's'} changed`, 'info') : null),
        groupRunPill(group.last_deployment)),
      h('div', { class: 'source muted truncate', title: `${group.repository_url} @ ${group.branch}` }, `${group.repository_url} @ ${group.branch}`),
      h('table', { class: 'table compact' },
        h('thead', {}, h('tr', {}, h('th', {}, 'App'), h('th', {}, 'Status'), h('th', {}, 'Version'), h('th', {}, ''))),
        h('tbody', {}, rows))),
    h('div', { class: 'actions' }, groupButtons(group, reload)));
}

/** The form fields of a group (name, repository, branch, on failure). */
function groupFields(group = {}) {
  const name = h('input', { id: 'group-name', value: group.name || '', autocomplete: 'off', required: true });
  const repo = h('input', { id: 'group-repo', class: 'mono', value: group.repository_url || '', placeholder: 'git@github.com:owner/repo.git', required: true });
  const branch = h('input', { id: 'group-branch', class: 'mono', value: group.branch || '', placeholder: 'main' });
  const rollback = h('input', { type: 'checkbox', id: 'group-rollback', checked: group.rollback_on_failure ?? true });
  const el = [
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'group-name' }, 'Group name'), name),
      h('div', { class: 'field' }, h('label', { for: 'group-branch' }, 'Branch'), branch)),
    h('div', { class: 'field' }, h('label', { for: 'group-repo' }, 'Repository URL'), repo,
      h('span', { class: 'hint' }, 'Every app in the group is deployed from this repository and branch.')),
    h('label', { class: 'small', for: 'group-rollback' }, rollback, ' When an app fails, roll back the apps this group deployment already switched'),
  ];
  const read = () => ({
    name: name.value.trim(),
    repository_url: repo.value.trim(),
    branch: branch.value.trim() || null,
    rollback_on_failure: rollback.checked,
  });
  return { el, read, repo, branch };
}

/** Apps that could join a group from `repo` @ `branch` (not in another group). */
function eligibleApps(projects, repo, branch, groupId) {
  const norm = (u) => String(u || '').trim().replace(/\/+$/, '').replace(/\.git$/, '').toLowerCase();
  return projects.filter((p) => p.repository_url && !p.source_dir
    && norm(p.repository_url) === norm(repo) && p.branch === (branch || 'main')
    && (!p.group || p.group.id === groupId));
}

async function newGroupPage(main, ctx) {
  const projects = await api('GET', '/projects');
  if (!ctx.alive()) return;
  const fields = groupFields();
  const picks = h('div', { 'data-group-picks': '' });
  const drawPicks = () => {
    const apps = eligibleApps(projects, fields.repo.value, fields.branch.value.trim(), null);
    replace(picks, apps.length
      ? h('fieldset', { class: 'field' }, h('legend', {}, 'Apps (deployed in this order; set watch paths on the group page)'),
        apps.map((p) => h('label', { class: 'small' }, h('input', { type: 'checkbox', value: p.id, 'aria-label': `Add ${p.name}` }), ` ${p.name}${p.source_subdir ? ` (${p.source_subdir})` : ''}`)))
      : h('p', { class: 'muted small' }, 'No app is deployed from this repository and branch yet (apps from a server folder or in another group are not listed).'));
  };
  fields.repo.addEventListener('input', drawPicks);
  fields.branch.addEventListener('input', drawPicks);
  drawPicks();
  const save = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'create-group' }, 'Create group');
  const form = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      const req = fields.read();
      req.members = [...picks.querySelectorAll('input:checked')].map((i) => ({ project: i.value, watch_paths: [] }));
      const res = await run(save, () => api('POST', '/groups', req), (g) => `Group ${g.name} created`);
      if (res) go(`/groups/${enc(res.name)}`);
    },
  },
  cardHeader('New group', 'groups'),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, 'A group deploys apps from one repository together: one commit, one clone, in order, and only the apps whose files changed.'),
    fields.el, picks,
    h('div', { class: 'form-actions' }, save)));
  append(main, h('a', { class: 'small', href: '#/' }, '← Overview'), pageHeader('New group', 'groups'), form);
}

async function groupPage(main, ctx, key) {
  let [group, projects] = await Promise.all([api('GET', `/groups/${enc(key)}`), api('GET', '/projects')]);
  if (!ctx.alive()) return;
  const header = h('div');
  const body = h('div', { class: 'grid' });
  const reload = async () => {
    try {
      [group, projects] = await Promise.all([api('GET', `/groups/${enc(group.id)}`), api('GET', '/projects')]);
      if (ctx.alive()) draw();
    } catch (err) {
      if (err.status === 404 && ctx.alive()) go('/');
    }
  };
  const saveMembers = async (btn, members, message) => {
    const res = await run(btn, () => api('PUT', `/groups/${enc(group.id)}/members`, { members }), message);
    if (res) await reload();
  };
  const memberList = () => group.members.map((m) => ({ project: m.project_id, watch_paths: m.watch_paths }));

  const draw = () => {
    replace(header, h('div', { class: 'page-header' },
      h('h1', {}, group.name), helpButton('groups'), groupRunPill(group.last_deployment),
      h('span', { 'data-group-updates': '' }),
      h('div', { class: 'spacer' }), groupButtons(group, reload, { small: false }),
      h('div', { class: 'subtitle muted small' }, `${group.repository_url} @ ${group.branch} · ${group.members.length} app${group.members.length === 1 ? '' : 's'}`)));

    const admin = can('admin');
    const rows = group.members.map((m, i) => {
      const watch = h('input', { class: 'mono', value: m.watch_paths.join(', '), placeholder: 'whole repository, e.g. frontend_v2/**, shared/**', 'aria-label': `Watch paths of ${m.name}`, disabled: !admin });
      const saveBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'save-watch' }, 'Save');
      saveBtn.addEventListener('click', () => {
        const list = memberList();
        list[i].watch_paths = watch.value.split(',').map((s) => s.trim()).filter(Boolean);
        saveMembers(saveBtn, list, `Watch paths of ${m.name} saved`);
      });
      const move = (delta, label) => {
        const btn = h('button', { class: 'btn small ghost', type: 'button', 'aria-label': `${label} ${m.name}`, disabled: i + delta < 0 || i + delta >= group.members.length }, delta < 0 ? '↑' : '↓');
        btn.addEventListener('click', () => {
          const list = memberList();
          const [item] = list.splice(i, 1);
          list.splice(i + delta, 0, item);
          saveMembers(btn, list, 'Order saved');
        });
        return btn;
      };
      const removeBtn = h('button', { class: 'btn small danger', type: 'button', 'data-action': 'remove-member' }, 'Remove');
      removeBtn.addEventListener('click', async () => {
        const ok = await confirmDialog({ title: `Remove ${m.name} from ${group.name}?`, message: 'The app stays; it is deployed on its own again (its own webhook applies again).', confirmLabel: 'Remove' });
        if (ok) saveMembers(removeBtn, memberList().filter((x) => x.project !== m.project_id), `${m.name} removed from the group`);
      });
      const last = group.last_deployment?.members.find((x) => x.project_id === m.project_id);
      return h('tr', { 'data-member': m.name },
        h('td', {}, `${i + 1}.`),
        h('td', {}, h('a', { href: `#/apps/${enc(m.name)}` }, m.name), m.source_subdir ? h('div', { class: 'muted small mono' }, m.source_subdir) : null),
        h('td', { class: 'mono small' }, m.current_release || '–', m.commit ? h('div', { class: 'muted' }, m.commit.slice(0, 10)) : null),
        h('td', { 'data-member-updates': '' }),
        h('td', {}, last ? pill(last.status, MEMBER_RUN_KIND[last.status] ?? '') : null, last?.reason ? h('div', { class: 'muted small' }, last.reason) : null),
        h('td', {}, h('div', { class: 'row' }, watch, admin ? saveBtn : null)),
        admin ? h('td', {}, move(-1, 'Move up'), move(1, 'Move down'), removeBtn) : null);
    });
    const membersCard = h('section', { class: 'card', 'data-card': 'group-members' },
      cardHeader('Apps', 'groups.members'),
      h('div', { class: 'card-body' },
        h('p', { class: 'muted small' }, 'Deployed in this order. An app with watch paths is only deployed by "Deploy changed only" (and the webhook) when a file matching them changed since its live commit; without watch paths, any change counts.'),
        group.members.length
          ? h('table', { class: 'table' },
            h('thead', {}, h('tr', {}, h('th', {}, '#'), h('th', {}, 'App'), h('th', {}, 'Live'), h('th', {}, 'Changes'), h('th', {}, 'Last group deployment'), h('th', {}, 'Watch paths'), admin ? h('th', {}, '') : null)),
            h('tbody', {}, rows))
          : h('p', {}, 'No apps yet.'),
        admin ? addMemberForm() : null));

    const settingsCard = admin ? groupSettingsCard() : null;
    const history = h('section', { class: 'card', 'data-card': 'group-history' },
      cardHeader('Group deployments', 'groups'),
      h('div', { class: 'card-body', 'data-group-history': '' }, h('p', { class: 'muted small' }, 'Loading…')));
    loadHistory(history.querySelector('[data-group-history]'));
    const commitsCard = h('section', { class: 'card', 'data-card': 'group-commits' },
      cardHeader(`New commits on ${group.branch}`, 'app.updates'),
      h('div', { class: 'card-body', 'data-group-commits': '' }, h('p', { class: 'muted small' }, 'Checking…')));
    replace(body, membersCard, commitsCard, history,
      admin ? hookCard(`/groups/${enc(group.id)}/webhook`, 'groups.webhook', 'this group') : null,
      settingsCard);
    drawGroupUpdates(ctx, group, main);
  };

  const addMemberForm = () => {
    const apps = eligibleApps(projects, group.repository_url, group.branch, group.id)
      .filter((p) => !group.members.some((m) => m.project_id === p.id));
    if (!apps.length) return h('p', { class: 'muted small' }, 'Other apps deployed from this repository and branch appear here to be added.');
    const pick = h('select', { id: 'group-add', 'aria-label': 'App to add' }, apps.map((p) => h('option', { value: p.id }, p.name)));
    const paths = h('input', { id: 'group-add-paths', class: 'mono', placeholder: 'watch paths, e.g. backend/**' });
    const add = h('button', { class: 'btn', type: 'button', 'data-action': 'add-member' }, 'Add app');
    add.addEventListener('click', async () => {
      const res = await run(add, () => api('POST', `/groups/${enc(group.id)}/members`, {
        project: pick.value,
        watch_paths: paths.value.split(',').map((s) => s.trim()).filter(Boolean),
      }), 'App added');
      if (res) reload();
    });
    return h('div', { class: 'row' }, pick, paths, add);
  };

  const groupSettingsCard = () => {
    const fields = groupFields(group);
    const save = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'save-group' }, 'Save group');
    const del = h('button', { class: 'btn danger', type: 'button', 'data-action': 'delete-group' }, 'Delete group');
    del.addEventListener('click', async () => {
      const ok = await confirmDialog({ title: `Delete group ${group.name}?`, message: 'Its apps stay and are deployed on their own again. The group\'s webhook stops working.', confirmLabel: 'Delete group', danger: true });
      if (!ok) return;
      const res = await run(del, () => api('DELETE', `/groups/${enc(group.id)}`), 'Group deleted');
      if (res !== undefined) go('/');
    });
    return h('form', {
      class: 'card', 'data-card': 'group-settings',
      onsubmit: async (e) => {
        e.preventDefault();
        const res = await run(save, () => api('PUT', `/groups/${enc(group.id)}`, fields.read()), 'Group saved');
        if (res) {
          if (res.name !== group.name) go(`/groups/${enc(res.name)}`);
          else reload();
        }
      },
    },
    cardHeader('Group settings', 'groups'),
    h('div', { class: 'card-body' }, fields.el, h('div', { class: 'form-actions' }, del, save)));
  };

  const loadHistory = async (el) => {
    try {
      const runs = (await api('GET', `/groups/${enc(group.id)}/deployments`)).reverse().slice(0, 10);
      if (!ctx.alive()) return;
      replace(el, runs.length
        ? h('ul', { class: 'deliveries' }, runs.map((r) => h('li', { 'data-run': r.id },
          timeEl(r.created_at, fmtAgo(r.created_at)), ' ', groupRunPill(r), ' ',
          r.commit ? h('span', { class: 'mono small' }, r.commit.slice(0, 10)) : null, ' ',
          h('span', { class: 'muted small' }, r.members.map((m) => `${m.name}: ${m.status}${m.version ? ` ${m.version}` : ''}`).join(' · ')),
          r.error ? h('div', { class: 'small' }, r.error) : null)))
        : h('p', { class: 'muted small' }, 'No group deployments yet.'));
    } catch (err) {
      if (ctx.alive()) replace(el, errorBox(err));
    }
  };

  draw();
  append(main, h('a', { class: 'small', href: '#/' }, '← Overview'), header, body);
  const later = debounce(reload, 300);
  onEvents(ctx, (e) => {
    if (e.payload?.group_id === group.id || group.members.some((m) => m.project_id === e.payload?.project_id)) later();
  });
  ctx.cleanup(later.cancel);
}

// ----------------------------------------------------------------------
// App detail
// ----------------------------------------------------------------------

const APP_TABS = [
  ['deploy', 'Deploy'],
  ['releases', 'Releases'],
  ['logs', 'Logs'],
  ['resources', 'Resources'],
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
      helpButton('app'),
      processPill(project),
      project.protected ? h('span', { class: 'pill info', title: 'Hidden behind a password; unlocked for this session', 'data-hidden-app': '' }, 'Hidden') : null,
      p && p.runtime === 'docker'
        ? h('span', { 'data-runtime': 'docker', title: p.image || '' }, pill('Docker', 'info'))
        : null,
      project.group
        ? h('a', { class: 'pill info', href: `#/groups/${enc(project.group.name)}`, 'data-group-link': '' }, `Group ${project.group.name}`)
        : null,
      h('div', { class: 'spacer' }),
      processButtons(project, refreshProject, { small: false }),
      h('div', { class: 'subtitle muted small' },
        [
          project.current_release ? `Version ${project.current_release}` : 'Not deployed yet',
          (project.instances || []).length > 1 ? `${project.instances.length} instances` : null,
          p && p.status === 'Running' ? `up ${uptime(p)}` : null,
          p ? `${p.restart_count} restart${p.restart_count === 1 ? '' : 's'}` : null,
          p && p.pid ? `${p.runtime === 'docker' ? 'container pid' : 'pid'} ${p.pid}` : null,
          p && p.runtime === 'docker' && p.image ? `image ${p.image}` : null,
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
    case 'resources': return resourcesTab(body, tabCtx);
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
    STRATEGIES.map(([value, label]) => h('option', { value }, label)));
  const preferred = project.settings?.strategy;
  if (preferred && STRATEGIES.some(([value]) => value === preferred)) strategy.value = preferred;
  const strategyHint = h('span', { class: 'hint', 'data-strategy-hint': '' });
  const syncStrategyHint = () => {
    strategyHint.textContent = STRATEGY_HINTS[strategy.value] || '';
  };
  strategy.addEventListener('change', syncStrategyHint);
  syncStrategyHint();
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
  cardHeader('New deployment', 'app.deploy'),
  h('div', { class: 'card-body' },
    h('div', { class: 'row' },
      gitSource ? h('div', { class: 'field' }, h('label', { for: 'deploy-branch' }, 'Branch'), branch) : null,
      gitSource ? h('div', { class: 'field' }, h('label', { for: 'deploy-commit' }, 'Commit'), commit, h('span', { class: 'hint' }, 'A commit SHA on that branch; blank for its latest commit')) : null,
      h('div', { class: 'field' }, h('label', { for: 'deploy-strategy' }, 'Strategy'), strategy, strategyHint)),
    gitSource ? null : h('p', { class: 'muted small' }, `Copies the code in ${project.source_dir || 'the source directory'}.`),
    h('div', { class: 'form-actions' }, submit)));

  append(body, h('div', { class: 'grid' }, form, updatesCard(ctx), instancesCard(ctx), pipelineEl));

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

/** The commits (and files) a deployment would bring, from the latest check. */
function commitList(u) {
  return [
    u.commits.length
      ? h('ul', { class: 'commit-list', 'data-commits': '' }, u.commits.map((c) => h('li', {},
        h('code', { class: 'mono' }, c.sha.slice(0, 8)),
        h('span', {}, c.message),
        h('span', { class: 'muted small' }, `${c.author} · `, timeEl(c.date, fmtAgo(c.date))))))
      : null,
    u.changed_files.length
      ? h('details', { class: 'small' }, h('summary', {}, `${u.changed_files.length} changed file${u.changed_files.length === 1 ? '' : 's'}`),
        h('ul', { class: 'mono small', 'data-files': '' }, u.changed_files.map((f) => h('li', {}, f))))
      : null,
  ];
}

/** Deploy tab: what's new on the branch (or in the folder) since the live release. */
function updatesCard(ctx) {
  const project = ctx.project();
  const out = h('div', { 'data-updates-card': '' }, h('p', { class: 'muted small' }, 'Checking…'));
  const checkBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'check-updates' }, 'Check now');
  const draw = (u) => {
    if (!ctx.alive()) return;
    const folder = u.kind === 'folder';
    let summary;
    if (u.error && u.behind == null) summary = h('div', { class: 'alert warn' }, u.error);
    else if (!u.deployed) summary = h('p', { class: 'small' }, 'Not deployed yet', u.head ? [' · latest ', h('code', {}, u.head.slice(0, 8))] : null);
    else if (u.up_to_date) summary = h('p', { class: 'small', 'data-up-to-date': '' }, pill('Up to date', 'ok'), ` with ${folder ? 'the folder' : u.branch} at `, h('code', {}, (u.head || '').slice(0, 8)));
    else {
      summary = h('p', { class: 'small' },
        u.behind > 0 ? pill(`${u.behind} new commit${u.behind === 1 ? '' : 's'}`, 'info') : null, ' ',
        folder ? 'in the folder' : `on ${u.branch}`, ' since the live release (', h('code', {}, (u.live_commit || '').slice(0, 8)), ')',
        u.dirty > 0 ? [' · ', pill(`${u.dirty} uncommitted file${u.dirty === 1 ? '' : 's'}`, 'warn')] : null);
    }
    const deployLatest = can('deployer') && u.deployed && !u.up_to_date && !project.group
      ? h('button', { class: 'btn small primary', type: 'button', 'data-action': 'deploy-latest' }, folder ? 'Deploy the folder' : 'Deploy latest')
      : null;
    if (deployLatest) {
      deployLatest.addEventListener('click', async () => {
        const req = folder || !u.head ? {} : { commit: u.head };
        const res = await run(deployLatest, () => api('POST', `/projects/${enc(project.id)}/deploy`, req), 'Deployment queued');
        if (res) go(`/apps/${enc(project.name)}/deploy`);
      });
    }
    replace(out, summary, commitList(u),
      project.group && !u.up_to_date && u.deployed
        ? h('p', { class: 'muted small' }, 'This app is in group ', h('a', { href: `#/groups/${enc(project.group.name)}` }, project.group.name), ': deploy it with the group.')
        : null,
      h('div', { class: 'form-actions' }, h('span', { class: 'muted small' }, `Checked ${fmtAgo(u.checked_at)}`), deployLatest));
  };
  checkBtn.addEventListener('click', async () => {
    const u = await run(checkBtn, () => api('POST', `/projects/${enc(project.id)}/updates/check`));
    if (u) draw(u);
  });
  api('GET', `/projects/${enc(project.id)}/updates`).then(draw).catch((err) => ctx.alive() && replace(out, errorBox(err)));
  return h('section', { class: 'card', 'data-card': 'updates' },
    cardHeader('New changes', 'app.updates', h('span', { class: 'spacer' }), can('deployer') ? checkBtn : null),
    h('div', { class: 'card-body' }, out));
}

/** Group page: per-app changes since live, "Check now" and "Deploy changed apps". */
function drawGroupUpdates(ctx, group, main) {
  const apply = (u) => {
    if (!ctx.alive()) return;
    for (const m of u.members) {
      const cell = main.querySelector(`tr[data-member="${CSS.escape(m.name)}"] [data-member-updates]`);
      if (!cell) continue;
      replace(cell,
        m.error && m.behind == null ? h('span', { title: m.error }, pill('Check failed', 'warn')) : null,
        !m.deployed ? pill('Not deployed', '') : null,
        m.deployed && m.relevant && !m.up_to_date
          ? h('span', { 'data-changed': '' }, pill(`Changed since live (${m.changed_files.length} file${m.changed_files.length === 1 ? '' : 's'})`, 'info'))
          : null,
        m.deployed && !m.relevant && m.behind > 0 ? h('span', { class: 'muted small' }, 'No watched changes') : null,
        m.deployed && m.up_to_date ? pill('Up to date', 'ok') : null);
    }
    const head = main.querySelector('[data-group-updates]');
    if (head) {
      const behind = Math.max(0, ...u.members.map((m) => m.behind ?? 0));
      head.dataset.groupUpdates = String(u.changed_apps);
      replace(head,
        behind > 0 ? pill(`${behind} new commit${behind === 1 ? '' : 's'} · ${u.changed_apps} app${u.changed_apps === 1 ? '' : 's'} changed`, 'info') : null,
        can('deployer') ? checkBtn : null);
    }
    const first = u.members.find((m) => m.commits.length);
    const list = main.querySelector('[data-group-commits]');
    if (list) replace(list, first ? commitList({ ...first, changed_files: [] }) : h('p', { class: 'muted small' }, u.error || 'No new commits.'));
  };
  const checkBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'check-group-updates' }, 'Check now');
  checkBtn.addEventListener('click', async () => {
    const u = await run(checkBtn, () => api('POST', `/groups/${enc(group.id)}/updates/check`));
    if (u) apply(u);
  });
  api('GET', `/groups/${enc(group.id)}/updates`).then(apply).catch(() => {});
}

const STRATEGIES = [
  ['GracefulSwitch', 'Graceful switch (zero downtime)'],
  ['Immediate', 'Immediate'],
  ['Rolling', 'Rolling (one instance at a time)'],
  ['BlueGreen', 'Blue-green (keep the old set warm)'],
];

const STRATEGY_HINTS = {
  GracefulSwitch: 'Starts the new release next to the live one and switches once it is healthy.',
  Immediate: 'Stops the live release first; requests fail until the new one is up.',
  Rolling: 'Replaces instances one batch at a time. Needs a domain on the built-in proxy.',
  BlueGreen: 'Switches all instances at once and keeps the old ones running for an instant rollback. Needs a domain on the built-in proxy.',
};

// --- Instances (deploy tab) ---------------------------------------------

/** The app's instances: status, pid, port, release, usage, logs. */
function instancesCard(ctx) {
  const project = ctx.project();
  const instances = project.instances || [];
  if (instances.length < 2 && !project.standby) return null;
  const rows = instances.map((inst, i) => {
    const p = inst.process;
    const n = p.instance ?? i + 1;
    return h('tr', { 'data-instance': String(n) },
      h('td', { 'data-label': 'Instance' }, `#${n}`),
      h('td', { 'data-label': 'Status' }, processPill({ process: p, current_release: project.current_release })),
      h('td', { class: 'mono', 'data-label': 'PID' }, p.pid ?? '–'),
      h('td', { class: 'mono', 'data-label': 'Port' }, p.port ?? '–'),
      h('td', { class: 'mono', 'data-label': 'Release' }, p.release_version || '–'),
      h('td', { class: 'num', 'data-label': 'Restarts' }, p.restart_count),
      h('td', { 'data-label': 'CPU' }, inst.cpu_cores != null ? fmtCores(inst.cpu_cores) : '–'),
      h('td', { 'data-label': 'Memory' }, inst.memory_bytes != null ? fmtBytes(inst.memory_bytes) : '–'),
      h('td', { class: 'actions' }, h('a', { class: 'btn small ghost', href: `#/apps/${enc(project.name)}/logs?process=${enc(p.id)}` }, 'Logs')));
  });
  return h('section', { class: 'card', 'data-instances': '' },
    cardHeader(`Instances (${instances.length})`, 'app.instances'),
    h('div', { class: 'card-body' },
      h('p', { class: 'muted small' }, 'The built-in proxy spreads requests over the running instances. Resource limits apply to each instance.'),
      h('div', { class: 'table-wrap' },
        h('table', { class: 'stack' },
          h('thead', {}, h('tr', {}, ['Instance', 'Status', 'PID', 'Port', 'Release', 'Restarts', 'CPU', 'Memory', ''].map((t) => h('th', {}, t)))),
          h('tbody', {}, rows))),
      project.standby
        ? h('div', { class: 'alert info', 'data-standby': '' },
          `Release ${project.standby.version} is still running on ${project.standby.process_ids.length} instance(s) until ${fmtTime(project.standby.until)}: rolling back to it now is instant.`)
        : null));
}

// --- Pipeline view (deploy tab and deployment detail) -------------------

const STAGES = ['Clone', 'Install', 'Build', 'Test', 'Package', 'Verify', 'Promote'];

const instanceList = (list) => (list || []).map((i) => `#${i.instance}${i.port ? ` (:${i.port})` : ''}`).join(', ');

/** Rollout steps (Rolling, BlueGreen, several instances) for the timeline. */
function rolloutSteps(events) {
  const steps = [];
  for (const e of events) {
    const p = e.payload || {};
    switch (e.event_type) {
      case 'RolloutStepCompleted':
        steps.push({
          kind: 'ok',
          at: e.created_at,
          text: `Step ${p.step} of ${p.steps}: started ${instanceList(p.started) || 'nothing'}`
            + (p.stopped && p.stopped.length ? `; drained and stopped ${p.stopped.length} old instance(s)` : ''),
        });
        break;
      case 'RolloutAborted':
        steps.push({ kind: 'bad', at: e.created_at, text: `Stopped at step ${p.step} of ${p.steps}: ${p.reason}. Restored ${(p.restored_instances || []).length} instance(s) of the previous release.` });
        break;
      case 'TrafficSwitched':
        steps.push({ kind: 'ok', at: e.created_at, text: `Traffic switched to ${p.to_version} on ${instanceList(p.instances)}` });
        break;
      case 'StandbyKept':
        steps.push({ kind: 'info', at: e.created_at, text: `${p.version} kept running until ${fmtTime(p.until)} for an instant rollback` });
        break;
      default:
        break;
    }
  }
  return steps;
}

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
        helpButton('app.pipeline'),
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
        (() => {
          const steps = rolloutSteps(events);
          if (!steps.length) return null;
          return h('ol', { class: 'rollout', 'aria-label': 'Rollout steps', 'data-rollout': '' },
            steps.map((s) => h('li', { class: `rollout-step ${s.kind}` },
              h('span', { class: 'muted small' }, fmtTime(s.at)), ' ', s.text)));
        })(),
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
    replace(body, h('div', { class: 'card' }, cardHeader('Releases', 'app.releases'), h('div', { class: 'table-wrap' },
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
              : null))))))));
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
  // A single instance's log: #/apps/<name>/logs?process=<id>.
  const wanted = new URLSearchParams(location.hash.split('?')[1] || '').get('process');
  const instances = project.instances || [];
  const target = instances.some((i) => i.process.id === wanted) ? wanted : project.name;
  if (instances.length > 1) {
    const pick = h('select', { id: 'log-instance', 'aria-label': 'Instance' },
      instances.map((i, n) => h('option', { value: i.process.id }, `Instance ${i.process.instance ?? n + 1}`)));
    pick.value = target === project.name ? instances[0].process.id : target;
    pick.addEventListener('change', () => go(`/apps/${enc(project.name)}/logs?process=${enc(pick.value)}`));
    append(body, h('div', { class: 'field inline' }, h('label', { for: 'log-instance' }, 'Instance'), pick));
  }
  append(body, logViewer(ctx, target));
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
      h('button', { class: 'btn small', type: 'button', 'data-action': 'download', onclick: download }, 'Download'),
      helpButton('app.logs')),
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

/** Rows of NAME = value inputs; `read()` returns an object or throws. */
function envEditor(values, idPrefix) {
  const rows = h('div', { class: 'env-rows' });
  const addRow = (name = '', value = '') => {
    const n = rows.children.length;
    const nameIn = h('input', { class: 'mono', value: name, placeholder: 'NAME', 'aria-label': `Variable ${n + 1} name`, id: `${idPrefix}-name-${n}` });
    const valueIn = h('input', { class: 'mono', value, placeholder: 'value', 'aria-label': `Variable ${n + 1} value` });
    const row = h('div', { class: 'env-row' }, nameIn, h('span', { class: 'muted' }, '='), valueIn,
      h('button', { class: 'btn small', type: 'button', 'aria-label': 'Remove variable', onclick: () => row.remove() }, 'Remove'));
    rows.append(row);
    return nameIn;
  };
  for (const [k, v] of Object.entries(values || {})) addRow(k, v);
  const addBtn = h('button', { class: 'btn small', type: 'button', onclick: () => addRow().focus() }, 'Add variable');
  const read = () => {
    const out = {};
    for (const row of rows.children) {
      const [nameIn, , valueIn] = row.children;
      const name = nameIn.value.trim();
      if (!name && !valueIn.value) continue;
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error(`'${name}' is not a valid variable name`);
      if (name in out) throw new Error(`${name} is listed twice`);
      out[name] = valueIn.value;
    }
    return out;
  };
  return { el: h('div', {}, rows, addBtn), read };
}

function diffView(diff) {
  return h('pre', { class: 'diff', 'aria-label': 'Changes to aegis.toml' },
    diff.map((l) => h('span', { class: l.op === '+' ? 'add' : l.op === '-' ? 'del' : '' }, `${l.op} ${l.text}\n`)));
}

const CONFIG_BASE = {
  source: 'the aegis.toml in the project directory',
  release: "the live release's aegis.toml",
  none: 'a new aegis.toml (the project has none yet)',
};

/** Shows the aegis.toml diff; resolves true when the operator saves. */
async function confirmConfigChange(preview) {
  const result = await modal(() =>
    h('form', { method: 'dialog', class: 'wide' },
      h('div', { class: 'dialog-body' },
        h('h2', {}, 'Review changes to aegis.toml'),
        h('p', { class: 'muted small' }, `Based on ${CONFIG_BASE[preview.base] || preview.base}. Every new release gets this file; copy it into your repository to keep the two in sync.`),
        diffView(preview.diff)),
      h('div', { class: 'form-actions' },
        copyButton(preview.proposed),
        h('span', { class: 'spacer' }),
        h('button', { class: 'btn', value: 'cancel', type: 'submit' }, 'Cancel'),
        h('button', { class: 'btn primary', value: 'ok', type: 'submit', 'data-confirm': '' }, 'Save settings'))));
  return result === 'ok';
}

async function showConfigFile(project, btn) {
  const preview = await run(btn, () => api('POST', `/projects/${enc(project.id)}/settings/preview`, project.settings || {}));
  if (!preview) return;
  await modal(() =>
    h('form', { method: 'dialog', class: 'wide' },
      h('div', { class: 'dialog-body' },
        h('h2', {}, 'aegis.toml for the next deployment'),
        h('p', { class: 'muted small' }, `Based on ${CONFIG_BASE[preview.base] || preview.base}, with the dashboard settings merged in. Secrets are not included.`),
        h('pre', { class: 'diff' }, preview.current)),
      h('div', { class: 'form-actions' },
        copyButton(preview.current),
        h('span', { class: 'spacer' }),
        h('button', { class: 'btn', value: 'close', type: 'submit' }, 'Close'))));
}

async function restartToApply(project, message) {
  if (!project.process || !['Running', 'Backoff'].includes(project.process.status)) return;
  const ok = await confirmDialog({ title: 'Restart the app now?', message, confirmLabel: 'Restart' });
  if (ok) await run(null, () => api('POST', `/processes/${enc(project.id)}/restart`), 'Restarted with the new values');
}

function secretsCard(ctx) {
  const project = ctx.project();
  const listBody = h('tbody');
  const nameIn = h('input', { id: 'secret-name', class: 'mono', placeholder: 'DATABASE_URL', required: true, autocomplete: 'off' });
  // A textarea so multi-line values (PEM keys, certificates) keep their
  // line breaks. It is masked where the browser supports it.
  const valueIn = h('textarea', {
    id: 'secret-value',
    class: 'mono secret-input',
    rows: 4,
    required: true,
    autocomplete: 'off',
    autocapitalize: 'off',
    autocorrect: 'off',
    spellcheck: 'false',
    'data-1p-ignore': true,
    'data-lpignore': 'true',
  });
  const canMask = typeof CSS !== 'undefined' && CSS.supports && CSS.supports('-webkit-text-security', 'disc');
  const showBtn = h('button', {
    class: 'btn small',
    type: 'button',
    'data-action': 'toggle-secret',
    'aria-controls': 'secret-value',
    'aria-pressed': 'false',
    hidden: !canMask,
  }, 'Show');
  const setMasked = (masked) => {
    valueIn.classList.toggle('masked', canMask && masked);
    showBtn.setAttribute('aria-pressed', masked ? 'false' : 'true');
    showBtn.textContent = masked ? 'Show' : 'Hide';
  };
  showBtn.addEventListener('click', () => setMasked(!valueIn.classList.contains('masked')));
  setMasked(true);
  const load = async () => {
    const secrets = await api('GET', `/projects/${enc(project.id)}/secrets`);
    const started = parseTime(ctx.project().process?.last_start);
    replace(listBody, secrets.length
      ? secrets.map((s) => {
        const stale = started !== null && parseTime(s.updated_at) > started;
        return h('tr', {},
          h('td', { 'data-label': 'Name', class: 'mono' }, s.name),
          h('td', { 'data-label': 'Updated' }, timeEl(s.updated_at, fmtAgo(s.updated_at)), stale ? [' ', pill('Restart to apply', 'warn')] : null),
          h('td', { class: 'actions' },
            h('button', { class: 'btn small', type: 'button', onclick: () => { nameIn.value = s.name; valueIn.focus(); } }, 'Replace'),
            ' ',
            h('button', {
              class: 'btn small danger',
              type: 'button',
              'data-action': 'delete-secret',
              onclick: async (e) => {
                const btn = e.currentTarget;
                const ok = await confirmDialog({ title: `Delete ${s.name}?`, message: 'The running app keeps the value until it restarts.', confirmLabel: 'Delete', danger: true });
                if (!ok) return;
                const done = await run(btn, () => api('DELETE', `/projects/${enc(project.id)}/secrets/${enc(s.name)}`), `${s.name} deleted`);
                if (done !== undefined) load().catch(() => {});
              },
            }, 'Delete')));
      })
      : h('tr', {}, h('td', { colspan: 3, class: 'muted small' }, 'No secrets yet.')));
  };
  const saveBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'save-secret' }, 'Save secret');
  const form = h('form', {
    class: 'card-body',
    onsubmit: async (e) => {
      e.preventDefault();
      const name = nameIn.value.trim();
      const res = await run(saveBtn, () => api('PUT', `/projects/${enc(project.id)}/secrets/${enc(name)}`, { value: valueIn.value }), `${name} saved`);
      if (!res) return;
      nameIn.value = '';
      valueIn.value = '';
      setMasked(true);
      await load().catch(() => {});
      if (res.restart_required) await restartToApply(ctx.project(), `${name} reaches the app when it restarts.`);
    },
  },
  h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'secret-name' }, 'Name'), nameIn)),
  h('div', { class: 'field' },
    h('label', { for: 'secret-value' }, 'Value'),
    valueIn,
    h('div', { class: 'field-tools' },
      h('span', { class: 'hint' }, canMask
        ? 'Saved exactly as entered, line breaks included (paste PEM keys as they are). Hidden while you type.'
        : 'Saved exactly as entered, line breaks included (paste PEM keys as they are).'),
      showBtn)),
  h('div', { class: 'form-actions' }, saveBtn));
  const card = h('section', { class: 'card' },
    cardHeader('Secrets', 'app.secrets'),
    h('div', { class: 'card-body' }, h('p', { class: 'muted small' }, 'Environment variables stored encrypted on the server and passed to the app when it starts. Values can be replaced but never read back.')),
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Name', 'Updated', ''].map((t) => h('th', {}, t)))),
      listBody)),
    form);
  load().catch((err) => replace(listBody, h('tr', {}, h('td', { colspan: 3 }, errorBox(err)))));
  return card;
}

// --- Environment: env file, sources, .env import (Settings tab) ---------

const ENV_SOURCES = {
  env_file: 'Env file',
  aegis_toml: 'aegis.toml [env]',
  variable: 'Variable',
  secret: 'Secret',
  aegis: 'Set by Aegis',
};

const ENV_PRECEDENCE = 'Lowest to highest: env file → [env] in aegis.toml → Variables → Secrets → PORT (always set by Aegis). Build commands get the env file, [env] and Variables, never Secrets.';

/** The app's environment: its env file on the server and where every
 * variable comes from (names only; values are never sent to the browser).
 * Admins can also set the env file and import a .env. `rerender` redraws
 * the Settings tab after a change. */
function envCard(ctx, rerender) {
  const project = ctx.project();
  const admin = can('admin');
  const status = h('div', {}, h('p', { class: 'muted small' }, 'Loading…'));
  const listBody = h('tbody');
  const pathIn = h('input', {
    id: 'env-file-path',
    class: 'mono',
    placeholder: '/opt/apps/my-app/.env',
    value: project.settings?.env_file || '',
    autocomplete: 'off',
    spellcheck: 'false',
  });
  const saveBtn = h('button', { class: 'btn', type: 'submit', 'data-action': 'save-env-file' }, 'Save env file');
  const importBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'import-env' }, 'Import .env…');
  importBtn.addEventListener('click', () => importEnvDialog(ctx.project(), rerender));

  const load = async () => {
    const o = await api('GET', `/projects/${enc(project.id)}/env`);
    const off = o.env_file_dirs.length === 0;
    const file = o.env_file;
    replace(status,
      off
        ? h('p', { class: 'muted small', 'data-env-file-off': '' },
          'Env files are turned off on this server. To read one, list the directories that may hold env files in the daemon configuration, e.g. ',
          h('code', {}, '[apps] env_file_dirs = ["/opt/apps"]'), ', and restart Aegis.')
        : h('p', { class: 'muted small' }, 'Allowed directories: ', o.env_file_dirs.map((d, i) => [i ? ', ' : '', h('code', {}, d)])),
      file
        ? h('div', { 'data-env-file-status': file.readable ? 'ok' : 'error' },
          h('dl', { class: 'facts' },
            h('div', {}, h('dt', {}, 'Env file'), h('dd', { class: 'mono small truncate', title: file.path }, file.path)),
            h('div', {}, h('dt', {}, 'Status'), h('dd', {}, file.readable ? pill('Readable', 'ok') : pill('Not readable', 'bad'))),
            h('div', {}, h('dt', {}, 'Variables'), h('dd', {}, file.readable ? String(file.names.length) : '–'))),
          file.error ? h('div', { class: 'alert', role: 'alert' }, file.error) : null)
        : null);
    if (admin) pathIn.disabled = off && !file;
    replace(listBody, o.variables.length
      ? o.variables.map((v) => h('tr', { 'data-env-var': v.name },
        h('td', { 'data-label': 'Name', class: 'mono' }, v.name),
        h('td', { 'data-label': 'Value from' }, pill(ENV_SOURCES[v.source] || v.source, v.source === 'secret' ? 'ok' : '')),
        h('td', { 'data-label': 'Overrides', class: 'small muted' }, v.overrides.map((s) => ENV_SOURCES[s] || s).join(', ') || '–'),
        h('td', { 'data-label': 'Build' }, v.build ? 'yes' : 'no')))
      : h('tr', {}, h('td', { colspan: 4, class: 'muted small' }, 'No variables yet.')));
  };

  const form = admin
    ? h('form', {
      class: 'card-body',
      onsubmit: async (e) => {
        e.preventDefault();
        const current = ctx.project();
        const path = pathIn.value.trim();
        if ((current.settings?.env_file || '') === path) {
          toast('Nothing to save');
          return;
        }
        const req = { ...(current.settings || {}), env_file: path || null };
        const res = await run(saveBtn, () => api('PUT', `/projects/${enc(current.id)}/settings`, req),
          path ? 'Env file saved; it is read at every build and start' : 'Env file removed');
        if (!res) return;
        await restartToApply(res, path
          ? 'The env file is read when the app starts.'
          : 'The app keeps the env file\'s values until it restarts.');
        rerender();
      },
    },
    h('div', { class: 'field' },
      h('label', { for: 'env-file-path' }, 'Env file path'),
      pathIn,
      h('span', { class: 'hint' }, 'An absolute path on the server, e.g. the app\'s existing .env. Read at every build and every start, never stored. Leave blank for none.')),
    h('div', { class: 'form-actions' }, importBtn, saveBtn))
    : null;

  const card = h('section', { class: 'card', 'data-card': 'environment' },
    cardHeader('Environment', 'app.env'),
    h('div', { class: 'card-body' },
      status,
      h('p', { class: 'muted small' }, ENV_PRECEDENCE)),
    form,
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack', 'aria-label': 'Environment variables and their sources' },
      h('thead', {}, h('tr', {}, ['Name', 'Value from', 'Overrides', 'Build'].map((t) => h('th', {}, t)))),
      listBody)));
  load().catch((err) => replace(status, errorBox(err)));
  return card;
}

const IMPORT_KINDS = [['variable', 'Variable'], ['secret', 'Secret'], ['skip', 'Skip']];

/** Import a .env pasted or read in the browser (FileReader; the file itself
 * is never uploaded). The server parses it and suggests a type per name;
 * values are never sent back. */
async function importEnvDialog(project, rerender) {
  const text = h('textarea', {
    id: 'env-import-text',
    class: 'mono secret-input',
    rows: 8,
    autocomplete: 'off',
    autocapitalize: 'off',
    autocorrect: 'off',
    spellcheck: 'false',
    'data-1p-ignore': true,
    'data-lpignore': 'true',
    placeholder: 'DATABASE_URL=postgres://…\nNEXT_PUBLIC_SITE_URL=https://…',
  });
  const canMask = typeof CSS !== 'undefined' && CSS.supports && CSS.supports('-webkit-text-security', 'disc');
  if (canMask) text.classList.add('masked');
  const fileIn = h('input', { id: 'env-import-file', type: 'file', accept: '.env,.txt,text/plain' });
  const out = h('div', { 'data-import-preview': '' });
  const previewBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'preview-import' }, 'Preview');
  const importBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'apply-import', disabled: true }, 'Import');
  let choices = null;
  let previewed = '';

  fileIn.addEventListener('change', () => {
    const file = fileIn.files && fileIn.files[0];
    if (!file) return;
    if (file.size > 1024 * 1024) {
      toast('That file is larger than 1 MiB', 'bad');
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      text.value = String(reader.result || '');
      previewBtn.click();
    };
    reader.onerror = () => toast('Could not read that file', 'bad');
    reader.readAsText(file);
  });
  text.addEventListener('input', () => {
    importBtn.disabled = true;
  });
  previewBtn.addEventListener('click', async () => {
    const content = text.value;
    const res = await run(previewBtn, () => api('POST', `/projects/${enc(project.id)}/env/import/preview`, { content }));
    if (!res) return;
    previewed = content;
    choices = {};
    const rows = res.variables.map((v) => {
      choices[v.name] = v.suggested;
      const select = h('select', { 'aria-label': `Import ${v.name} as` },
        IMPORT_KINDS.map(([value, label]) => h('option', { value, disabled: value === 'variable' && v.multiline }, label)));
      select.value = v.suggested;
      const note = h('td', { 'data-label': 'Note', class: 'small muted' });
      const describe = () => {
        const kind = select.value;
        const parts = [v.reason];
        if (v.exists_as && kind !== 'skip') {
          parts.push(v.exists_as === kind
            ? `replaces the existing ${kind}`
            : (kind === 'secret' ? 'moves it from Variables to Secrets' : 'a secret with this name still wins at runtime'));
        }
        replace(note, v.exists_as ? [pill(`exists as ${v.exists_as}`, 'warn'), ' '] : null, parts.join('; '));
      };
      select.addEventListener('change', () => {
        choices[v.name] = select.value;
        describe();
      });
      describe();
      return h('tr', { 'data-import-row': v.name },
        h('td', { 'data-label': 'Name', class: 'mono' }, v.name),
        h('td', { 'data-label': 'Import as' }, select),
        note);
    });
    replace(out, rows.length
      ? h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
        h('thead', {}, h('tr', {}, ['Name', 'Import as', 'Note'].map((t) => h('th', {}, t)))),
        h('tbody', {}, rows)))
      : h('p', { class: 'muted small' }, 'No variables found.'));
    importBtn.disabled = rows.length === 0;
  });

  let imported = null;
  await modal((dlg) => {
    importBtn.addEventListener('click', async () => {
      if (!choices || text.value !== previewed) {
        toast('Preview the changed text first', 'bad');
        return;
      }
      const res = await run(importBtn, () => api('POST', `/projects/${enc(project.id)}/env/import`, { content: previewed, choices }));
      if (!res) return;
      imported = res;
      text.value = '';
      dlg.close('done');
    });
    return h('form', { method: 'dialog', class: 'wide', 'aria-label': 'Import .env' },
      h('div', { class: 'dialog-body' },
        h('h2', {}, 'Import a .env file'),
        h('p', { class: 'muted small' }, 'Paste the file or choose it; it is read in this browser and sent to Aegis only to be parsed and imported. Pick Variable (written into each release, visible to viewers, available to builds) or Secret (encrypted, start only) for each name. Values are never shown again.'),
        h('div', { class: 'field' }, h('label', { for: 'env-import-text' }, 'Paste a .env file'), text),
        h('div', { class: 'field' }, h('label', { for: 'env-import-file' }, 'Or choose a file'), fileIn),
        out),
      h('div', { class: 'form-actions' },
        previewBtn,
        h('span', { class: 'spacer' }),
        h('button', { class: 'btn', value: 'cancel', type: 'submit' }, 'Cancel'),
        importBtn));
  });
  if (!imported) return;
  toast(`Imported ${imported.variables.length} variable(s) and ${imported.secrets.length} secret(s)`);
  if (imported.variables.length) toast('Variables apply to the next deployment');
  if (imported.restart_required) await restartToApply(project, 'Secrets reach the app when it restarts.');
  rerender();
}

const DELIVERY_KIND = { queued: 'ok', pong: 'ok', ignored: '', duplicate: '', error: 'bad' };

function webhookCard(ctx) {
  const project = ctx.project();
  if (!project.repository_url) {
    return h('section', { class: 'card' },
      cardHeader('GitHub webhook', 'app.webhook'),
      h('div', { class: 'card-body' }, h('p', { class: 'muted small' }, 'Webhooks need a git repository as the source.')));
  }
  if (project.group) {
    return h('section', { class: 'card', 'data-card': 'webhook' },
      cardHeader('GitHub webhook', 'app.webhook'),
      h('div', { class: 'card-body' },
        h('p', { class: 'muted small' }, 'This app is in group ',
          h('a', { href: `#/groups/${enc(project.group.name)}` }, project.group.name),
          ". Pushes deploy it through the group's webhook; pushes to this app's own webhook are ignored.")));
  }
  return hookCard(`/projects/${enc(project.id)}/webhook`, 'app.webhook', 'this app');
}

/** Webhook secret, payload URL, test delivery and recent deliveries. */
function hookCard(base, help, what) {
  const status = h('div', {}, h('p', { class: 'muted small' }, 'Loading…'));
  const hookOut = h('div');
  const hookBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'webhook' }, 'Generate secret');
  const load = async () => {
    const hook = await api('GET', base);
    const testBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'test-webhook', disabled: !hook.configured }, 'Send test delivery');
    testBtn.addEventListener('click', async () => {
      const res = await run(testBtn, () => api('POST', `${base}/test`));
      if (res) toast(res.status === 'pong' ? 'Test delivery accepted' : `Test delivery: ${res.status}`, res.status === 'pong' ? '' : 'bad');
      load().catch(() => {});
    });
    hookBtn.textContent = hook.configured ? 'Rotate secret' : 'Generate secret';
    replace(status,
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Status'), h('dd', {}, hook.configured ? pill('Configured', 'ok') : pill('No secret yet', 'warn'))),
        h('div', {}, h('dt', {}, 'Deploys on push to'), h('dd', { class: 'mono' }, hook.branch))),
      h('div', { class: 'field' }, h('span', { class: 'small muted' }, 'Payload URL'), secretBox(`${location.origin}${hook.path}`, 'Payload URL')),
      h('p', { class: 'muted small' }, `Content type ${hook.content_type}; events: ${hook.events.join(', ')}. The server must be reachable from GitHub.`),
      h('div', { class: 'form-actions' }, testBtn, hookBtn),
      hook.recent.length
        ? h('div', {}, h('h3', { class: 'small' }, 'Recent deliveries'),
          h('ul', { class: 'deliveries' }, hook.recent.map((d) => h('li', {},
            timeEl(d.received_at, fmtAgo(d.received_at)), ' ',
            h('span', { class: 'mono' }, d.event), ' ',
            pill(d.status, DELIVERY_KIND[d.status] ?? ''),
            d.reason ? h('span', { class: 'muted small' }, ` ${d.reason}`) : null))))
        : h('p', { class: 'muted small' }, 'No deliveries yet.'));
  };
  hookBtn.addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: 'Generate a new webhook secret?',
      message: `If ${what} already has a secret, it stops working; update the webhook on GitHub with the new one.`,
      confirmLabel: 'Generate',
    });
    if (!ok) return;
    const res = await run(hookBtn, () => api('POST', base));
    if (!res) return;
    replace(hookOut, h('div', { class: 'grid' },
      h('div', { class: 'alert warn' }, 'Copy the secret now; it is not shown again.'),
      h('div', {}, h('div', { class: 'small muted' }, 'Secret'), secretBox(res.secret, 'Webhook secret'))));
    load().catch(() => {});
  });
  load().catch((err) => replace(status, errorBox(err)));
  return h('section', { class: 'card', 'data-card': 'webhook' },
    cardHeader('GitHub webhook', help),
    h('div', { class: 'card-body' }, status, hookOut));
}

async function settingsTab(body, ctx) {
  const project = ctx.project();
  const rerender = async () => {
    await ctx.refreshProject();
    if (!body.isConnected) return;
    body.replaceChildren();
    settingsTab(body, ctx);
  };
  if (!can('admin')) {
    const viewBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'show-config' }, 'View aegis.toml');
    viewBtn.addEventListener('click', () => showConfigFile(ctx.project(), viewBtn));
    append(body, h('div', { class: 'grid' },
      h('section', { class: 'card' },
        cardHeader('Settings', 'app.settings'),
        h('div', { class: 'card-body' },
          h('p', { class: 'muted small' }, "Only admins can change an app's build and run settings, variables, secrets, domains, schedule and webhook."),
          viewBtn)),
      envCard(ctx, rerender)));
    return;
  }
  const s = project.settings || {};
  const { inputs, layout } = settingsInputs(s);
  const strategyIn = h('select', { id: 'set-strategy', name: 'strategy' },
    h('option', { value: '' }, 'From aegis.toml (GracefulSwitch by default)'),
    STRATEGIES.map(([value, label]) => h('option', { value }, label)));
  strategyIn.value = s.strategy || '';
  const instancesIn = h('input', { id: 'set-instances', name: 'instances', type: 'number', min: 1, max: 16, value: s.instances ?? '', placeholder: '1' });
  const healthTimeoutIn = h('input', { id: 'set-health-timeout', name: 'health_check_timeout_secs', type: 'number', min: 1, max: 3600, step: 1, value: s.health_check_timeout_secs ?? '', placeholder: '30' });
  const drainTimeoutIn = h('input', { id: 'set-drain-timeout', name: 'drain_timeout_secs', type: 'number', min: 0, max: 3600, step: 1, value: s.drain_timeout_secs ?? '', placeholder: '10' });
  const env = envEditor(s.env, 'env');
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
  const fileBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'show-config' }, 'View aegis.toml');
  fileBtn.addEventListener('click', () => showConfigFile(ctx.project(), fileBtn));

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
      if (strategyIn.value) req.strategy = strategyIn.value;
      if (instancesIn.value) req.instances = Number(instancesIn.value);
      // Every field is sent: a settings save clears what it leaves out.
      for (const [input, key, min, label] of [
        [healthTimeoutIn, 'health_check_timeout_secs', 1, 'Health check timeout'],
        [drainTimeoutIn, 'drain_timeout_secs', 0, 'Drain timeout'],
      ]) {
        const raw = input.value.trim();
        if (!raw) continue;
        const n = Number(raw);
        if (!Number.isInteger(n) || n < min || n > 3600) {
          toast(`${label} must be a whole number of seconds from ${min} to 3600`, 'bad');
          input.focus();
          return;
        }
        req[key] = n;
      }
      if (processOnly.checked) req.health_check_url = '';
      else if (inputs.health_check_url.value.trim()) req.health_check_url = inputs.health_check_url.value.trim();
      try {
        req.env = env.read();
      } catch (err) {
        toast(err.message, 'bad');
        return;
      }
      // Set in the Environment card; kept as it is here.
      if (ctx.project().settings?.env_file) req.env_file = ctx.project().settings.env_file;
      // Show what the change does to aegis.toml before saving it.
      const preview = await run(saveBtn, () => api('POST', `/projects/${enc(project.id)}/settings/preview`, req));
      if (!preview) return;
      if (!preview.changed) {
        toast('Nothing to save');
        return;
      }
      if (!(await confirmConfigChange(preview))) return;
      await run(saveBtn, () => api('PUT', `/projects/${enc(project.id)}/settings`, req), 'Settings saved; they apply to the next deployment');
      ctx.refreshProject();
    },
  },
  cardHeader('Build & run', 'app.settings', h('span', { class: 'spacer' }), fileBtn, detectBtn),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, 'Leave a field blank to use the value from aegis.toml or auto-detection. Changes take effect on the next deployment; older releases keep the settings they were built with.'),
    layout,
    h('label', { class: 'small', for: 'set-process-only' }, processOnly, ' No HTTP health check (healthy while the process stays up)'),
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'set-strategy' }, 'Default strategy'), strategyIn,
        h('span', { class: 'hint' }, 'Used when a deployment does not pick one. Rolling and Blue-green need a domain on the built-in proxy.')),
      h('div', { class: 'field' }, h('label', { for: 'set-instances' }, 'Instances'), instancesIn,
        h('span', { class: 'hint' }, 'Copies of the app behind the built-in proxy (1-16; more than 1 needs a domain). Resource limits apply to each instance.'))),
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'set-health-timeout' }, 'Health check timeout (seconds)'), healthTimeoutIn,
        h('span', { class: 'hint' }, 'How long a new release may take to become healthy before the deployment fails (1-3600; default 30). Raise it for apps that start slowly.')),
      h('div', { class: 'field' }, h('label', { for: 'set-drain-timeout' }, 'Drain timeout (seconds)'), drainTimeoutIn,
        h('span', { class: 'hint' }, 'How long a stopping release gets to finish requests and exit before it is killed (0-3600; default 10).'))),
    h('fieldset', { class: 'field env' },
      h('legend', {}, 'Environment variables'),
      h('span', { class: 'hint' }, 'Merged over [env] in aegis.toml and over the env file, and written into each release. Use Secrets for passwords and keys.'),
      env.el),
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
  cardHeader('Daily deploy', 'app.schedule'),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, sched ? `Deploys every day at ${time.value} UTC.` : 'Not scheduled.'),
    h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'sched-time' }, 'Time (UTC)'), time),
      project.repository_url ? h('div', { class: 'field' }, h('label', { for: 'sched-branch' }, 'Branch'), schedBranch) : null),
    h('div', { class: 'form-actions' }, schedBtn)));

  const info = h('section', { class: 'card' },
    cardHeader('About', 'app.about'),
    h('div', { class: 'card-body' },
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Project id'), h('dd', { class: 'mono small truncate', title: project.id }, project.id)),
        h('div', {}, h('dt', {}, 'Runtime'), h('dd', {}, project.runtime || 'auto')),
        h('div', { 'data-fact': 'runs-as' }, h('dt', {}, 'Runs as'), h('dd', { class: 'truncate', title: project.process?.image || '' },
          project.process?.runtime === 'docker'
            ? `Docker container${project.process.image ? ` (${project.process.image})` : ''}`
            : (project.process ? 'Process' : '–'))),
        h('div', {}, h('dt', {}, 'Created'), h('dd', {}, timeEl(project.created_at)))),
      h('p', { class: 'muted small' }, 'Source: ', sourceText(project))));

  append(body, h('div', { class: 'grid' }, settingsForm, repositoryCard(ctx, rerender), envCard(ctx, rerender), domainsCard(ctx), secretsCard(ctx), h('div', { class: 'grid two' }, scheduleForm, webhookCard(ctx)), info, hideCard(ctx, rerender), dangerCard(ctx)));
}

/** Asks for an app's name and hide password; the app then shows for this session. */
async function unlockAppDialog(after) {
  let name = '';
  let password = '';
  const result = await modal(() => {
    const nameIn = h('input', { id: 'unlock-name', class: 'mono', autocomplete: 'off', spellcheck: 'false', required: true, 'aria-label': 'App name' });
    const passIn = h('input', { id: 'unlock-password', type: 'password', autocomplete: 'off', required: true, 'aria-label': 'Hide password' });
    const form = h('form', {
      method: 'dialog',
      onsubmit: () => {
        name = nameIn.value.trim();
        password = passIn.value;
      },
    },
    h('div', { class: 'dialog-body' },
      h('h2', {}, 'Unlock a hidden app'),
      h('p', { class: 'muted' }, 'Hidden apps are left out of every list. Enter the app\'s name and its hide password to see it until you sign out.'),
      h('div', { class: 'field' }, h('label', { for: 'unlock-name' }, 'App name'), nameIn),
      h('div', { class: 'field' }, h('label', { for: 'unlock-password' }, 'Hide password'), passIn)),
    h('div', { class: 'form-actions' },
      h('button', { class: 'btn', value: 'cancel', type: 'submit', formnovalidate: true }, 'Cancel'),
      h('button', { class: 'btn primary', value: 'ok', type: 'submit', 'data-confirm': '' }, 'Unlock')));
    setTimeout(() => nameIn.focus(), 0);
    return form;
  });
  if (result !== 'ok' || !name) return;
  const done = await run(null, () => api('POST', `/projects/${enc(name)}/unlock`, { password }), `${name} unlocked`);
  if (done !== undefined) {
    if (after) after();
    go(`/apps/${enc(name)}`);
  }
}

/** Settings → Hide this app: hide it behind a password, or lock/unhide it. */
function hideCard(ctx, rerender) {
  const project = ctx.project();
  if (!can('admin')) return null;
  if (project.protected) {
    const lockBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'lock-app' }, 'Hide it now');
    lockBtn.addEventListener('click', async () => {
      const done = await run(lockBtn, () => api('POST', `/projects/${enc(project.id)}/lock`, {}), `${project.name} is hidden again`);
      if (done !== undefined) go('/');
    });
    const unhideBtn = h('button', { class: 'btn danger', type: 'button', 'data-action': 'unhide-app' }, 'Stop hiding');
    unhideBtn.addEventListener('click', async () => {
      const ok = await confirmDialog({
        title: `Stop hiding ${project.name}?`,
        message: 'Everyone who may see it will see it again without a password.',
        confirmLabel: 'Stop hiding',
        danger: true,
      });
      if (!ok) return;
      const done = await run(unhideBtn, () => api('DELETE', `/projects/${enc(project.id)}/hide`), `${project.name} is no longer hidden`);
      if (done !== undefined) rerender();
    });
    return h('section', { class: 'card', 'data-card': 'hide-app' },
      cardHeader('Hidden app', 'app.hide'),
      h('div', { class: 'card-body' },
        h('p', { class: 'muted small' }, 'This app is hidden behind a password. It is unlocked for your session only; other sessions must enter the password to see it.'),
        h('div', { class: 'form-actions' }, lockBtn, ' ', unhideBtn)));
  }
  const passIn = h('input', { id: 'hide-password', type: 'password', autocomplete: 'new-password', minlength: 12, required: true, 'aria-label': 'Hide password' });
  const hideBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'hide-app' }, 'Hide app');
  const form = h('form', {
    class: 'card-body',
    onsubmit: async (e) => {
      e.preventDefault();
      const done = await run(hideBtn, () => api('PUT', `/projects/${enc(project.id)}/hide`, { password: passIn.value }), `${project.name} is hidden`);
      if (done !== undefined) rerender();
    },
  },
  h('p', { class: 'muted small' }, 'Hide this app from every list, search and event feed. Anyone who wants to see it must enter the password (Overview → Unlock hidden app) in each new session.'),
  h('div', { class: 'field' }, h('label', { for: 'hide-password' }, 'Password (at least 12 characters)'), passIn,
    h('span', { class: 'hint' }, 'It cannot be recovered; keep it in a password manager.')),
  h('div', { class: 'form-actions' }, hideBtn));
  return h('section', { class: 'card', 'data-card': 'hide-app' }, cardHeader('Hide this app', 'app.hide'), form);
}

/** Settings → Danger zone: delete the app (typing its name to confirm). */
function dangerCard(ctx) {
  const project = ctx.project();
  const deleteBtn = h('button', { class: 'btn danger', type: 'button', 'data-action': 'delete-app' }, 'Delete app');
  deleteBtn.addEventListener('click', async () => {
    let keepReleases = false;
    const result = await modal(() => {
      const nameIn = h('input', { id: 'delete-confirm', class: 'mono', autocomplete: 'off', spellcheck: 'false', 'aria-label': 'App name' });
      const keep = h('input', { type: 'checkbox', id: 'delete-keep-releases', name: 'keep_releases' });
      const ok = h('button', { class: 'btn danger solid', value: 'ok', type: 'submit', disabled: true, 'data-confirm': '' }, 'Delete this app');
      nameIn.addEventListener('input', () => {
        ok.disabled = nameIn.value.trim() !== project.name;
      });
      const form = h('form', {
        method: 'dialog',
        onsubmit: () => {
          keepReleases = keep.checked;
        },
      },
      h('div', { class: 'dialog-body' },
        h('h2', {}, `Delete ${project.name}?`),
        h('p', { class: 'muted' }, 'Its processes are stopped (requests in flight get the drain timeout to finish), and its domains, secrets, deploy key, webhook secret, schedule, releases and logs are removed. Its history stays in Events. This can\'t be undone.'),
        (project.domains || []).length
          ? h('p', { class: 'small' }, 'Domains that stop being served: ', project.domains.map((d, i) => [i ? ', ' : '', h('code', {}, d.domain)]))
          : null,
        h('label', { class: 'small', for: 'delete-keep-releases' }, keep, ' Keep the release directories and logs on the server'),
        h('div', { class: 'field' }, h('label', { for: 'delete-confirm' }, 'Type ', h('code', {}, project.name), ' to confirm'), nameIn)),
      h('div', { class: 'form-actions' },
        h('button', { class: 'btn', value: 'cancel', type: 'submit', formnovalidate: true }, 'Cancel'),
        ok));
      setTimeout(() => nameIn.focus(), 0);
      return form;
    });
    if (result !== 'ok') return;
    const query = `confirm=${enc(project.name)}${keepReleases ? '&keep_releases=true' : ''}`;
    const res = await run(deleteBtn, () => api('DELETE', `/projects/${enc(project.id)}?${query}`),
      (r) => `Deleted ${r.name}${r.domains.length ? `; ${r.domains.join(', ')} can be used again` : ''}`);
    if (res) go('/');
  });
  return h('section', { class: 'card danger-zone', 'data-card': 'danger-zone' },
    cardHeader('Danger zone', 'app.delete'),
    h('div', { class: 'card-body' },
      h('p', { class: 'muted small' }, 'Deleting the app stops it and removes its domains, secrets, deploy key, webhook secret, schedule and releases. Its name and domains can be used again right away. Not possible while a deployment is running.'),
      h('div', { class: 'form-actions' }, deleteBtn)));
}

// --- Repository and deploy key (Settings tab, wizard) ------------------

/** `img-man` from `git@github.com:acme/img-man.git` (or any URL/path). */
function repoName(url) {
  const last = String(url || '').replace(/\/+$/, '').split(/[/:]/).pop() || 'app';
  return last.replace(/\.git$/, '') || 'app';
}

/**
 * A project's SSH deploy key: generate, show the public key with the steps
 * to add it to GitHub, test access, replace or delete. The private key
 * never reaches the browser.
 */
function deployKeyPanel(project, ctx) {
  const base = `/projects/${enc(project.id)}/deploy-key`;
  const el = h('div', { class: 'grid', 'data-deploy-key': '' }, h('p', { class: 'muted small' }, 'Loading…'));
  const result = h('div', { 'data-key-test': '' });
  const alive = () => !ctx || ctx.alive();

  const showTest = (c) => {
    replace(result, h('div', { class: `alert ${c.ok ? 'ok' : 'warn'}`, role: 'status', 'data-access': c.ok ? 'ok' : 'denied' },
      h('p', {}, c.ok
        ? `Access works: Aegis can read ${c.url}${c.branches.length ? ` (${c.branches.length} branch${c.branches.length === 1 ? '' : 'es'})` : ''}.`
        : `Aegis could not read ${c.url}.`),
      c.error ? h('pre', { class: 'small mono' }, c.error) : null,
      c.hint ? h('p', { class: 'small' }, c.hint) : null));
  };

  const draw = (key) => {
    const genBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'generate-key' }, key ? 'Generate a new key' : 'Generate deploy key');
    genBtn.addEventListener('click', async () => {
      if (key) {
        const ok = await confirmDialog({
          title: 'Replace the deploy key?',
          message: 'A new key pair is generated and the old private key is deleted. Add the new public key to the repository and remove the old one there.',
          confirmLabel: 'Replace key',
          danger: true,
        });
        if (!ok) return;
      }
      const res = await run(genBtn, () => api('POST', base, { replace: !!key }), key ? 'New deploy key generated' : 'Deploy key generated');
      if (res && alive()) draw(res);
    });
    if (!key) {
      replace(el,
        h('p', { class: 'muted small' }, 'No deploy key. For a private repository, generate one: Aegis keeps the private key encrypted on this server and shows you the public key to add to the repository.'),
        h('div', { class: 'form-actions' }, genBtn));
      return;
    }
    const testBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'test-key' }, 'Test access');
    testBtn.addEventListener('click', async () => {
      const c = await run(testBtn, () => api('POST', `${base}/test`, {}));
      if (c && alive()) showTest(c);
    });
    const delBtn = h('button', { class: 'btn danger', type: 'button', 'data-action': 'delete-key' }, 'Delete key');
    delBtn.addEventListener('click', async () => {
      const ok = await confirmDialog({
        title: 'Delete the deploy key?',
        message: 'Deployments of a private repository fail until a new key is added. Also remove the key from the repository\'s deploy keys.',
        confirmLabel: 'Delete key',
        danger: true,
      });
      if (!ok) return;
      const done = await run(delBtn, () => api('DELETE', base), 'Deploy key deleted');
      if (done !== undefined && alive()) draw(null);
    });
    const https = /^https?:\/\//.test(project.repository_url || '');
    replace(el,
      h('div', { class: 'field' },
        h('span', { class: 'small muted' }, 'Public key'),
        h('div', { class: 'secret', 'aria-label': 'Public key', 'data-public-key': '' }, h('code', {}, key.public_key), copyButton(key.public_key))),
      h('dl', { class: 'facts' },
        h('div', {}, h('dt', {}, 'Fingerprint'), h('dd', { class: 'mono small', 'data-fingerprint': '' }, key.fingerprint)),
        h('div', {}, h('dt', {}, 'Created'), h('dd', {}, timeEl(key.created_at)))),
      key.clone_url
        ? (https ? h('p', { class: 'small muted' }, 'Deploy keys only work over SSH, so deployments clone ', h('code', {}, key.clone_url), ' with this key.') : null)
        : h('div', { class: 'alert warn' }, 'This app\'s source is not an SSH or GitHub repository URL, so the key is not used. Use an ssh://… or git@host:owner/repo.git URL.'),
      h('ol', { class: 'update-steps small' },
        h('li', {}, 'Open ', key.add_key_url
          ? h('a', { href: key.add_key_url, target: '_blank', rel: 'noopener noreferrer', 'data-add-key-link': '' }, 'the repository\'s "Add deploy key" page')
          : 'the repository\'s deploy key settings', '.'),
        h('li', {}, 'Title: e.g. ', h('code', {}, `Aegis ${project.name}`), '. Key: paste the public key above (Copy).'),
        h('li', {}, 'Leave "Allow write access" unticked (Aegis only reads), then click Add key.'),
        h('li', {}, 'Click Test access below.')),
      h('div', { class: 'form-actions' }, testBtn, genBtn, delBtn),
      result);
  };

  api('GET', base)
    .then((key) => alive() && draw(key))
    .catch((err) => {
      if (!alive()) return;
      if (err.status === 404) draw(null);
      else replace(el, errorBox(err));
    });
  return el;
}

/** Settings → Repository: the app's source and its deploy key. */
function repositoryCard(ctx, rerender) {
  const project = ctx.project();
  const facts = h('dl', { class: 'facts' },
    h('div', {}, h('dt', {}, 'Source'), h('dd', { class: 'mono small truncate', title: sourceText(project), 'data-source': '' }, sourceText(project))),
    project.source_subdir ? h('div', {}, h('dt', {}, 'App folder'), h('dd', { class: 'mono small' }, project.source_subdir)) : null);
  const changeBtn = h('button', { class: 'btn', type: 'button', 'data-action': 'change-source' }, 'Change source');
  const changePanel = h('div', { 'data-change-source': '' });
  changeBtn.addEventListener('click', () => {
    if (changePanel.firstChild) {
      changePanel.replaceChildren();
      return;
    }
    const git = !project.source_dir;
    const kindGit = h('input', { type: 'radio', name: 'source-kind', id: 'source-kind-git', value: 'git', checked: git });
    const kindDir = h('input', { type: 'radio', name: 'source-kind', id: 'source-kind-dir', value: 'dir', checked: !git });
    const repoIn = h('input', { id: 'source-repo', class: 'mono', value: project.repository_url || '', placeholder: 'git@github.com:owner/repo.git', autocomplete: 'off', spellcheck: 'false' });
    const branchIn = h('input', { id: 'source-branch', class: 'mono', value: project.repository_url ? project.branch : '', placeholder: project.branch || 'main' });
    const dirIn = h('input', { id: 'source-dir', class: 'mono', value: project.source_dir || '', placeholder: '/opt/apps/my-app', autocomplete: 'off', spellcheck: 'false' });
    const subIn = h('input', { id: 'source-subdir', class: 'mono', value: project.source_subdir || '', placeholder: '(the whole source)' });
    const gitFields = h('div', { class: 'row' },
      h('div', { class: 'field' }, h('label', { for: 'source-repo' }, 'Repository URL'), repoIn),
      h('div', { class: 'field' }, h('label', { for: 'source-branch' }, 'Branch'), branchIn));
    const dirFields = h('div', { class: 'field' }, h('label', { for: 'source-dir' }, 'Server folder path'), dirIn,
      h('span', { class: 'hint' }, 'An absolute path the Aegis daemon can read. Each deployment copies it.'));
    const sync = () => {
      gitFields.hidden = !kindGit.checked;
      dirFields.hidden = kindGit.checked;
    };
    kindGit.addEventListener('change', sync);
    kindDir.addEventListener('change', sync);
    sync();
    const saveBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'save-source' }, 'Save source');
    const form = h('form', {
      onsubmit: async (e) => {
        e.preventDefault();
        const req = kindGit.checked
          ? { repository_url: repoIn.value.trim(), branch: branchIn.value.trim() || null }
          : { source_dir: dirIn.value.trim() };
        req.source_subdir = subIn.value.trim() || null;
        const ok = await confirmDialog({
          title: 'Change the source?',
          message: 'The next deployment builds from the new source. The running release keeps serving until then.'
            + (kindGit.checked ? '' : ' The deploy key (if any) is kept but not used for a folder.'),
          confirmLabel: 'Change source',
        });
        if (!ok) return;
        const res = await run(saveBtn, () => api('PUT', `/projects/${enc(project.id)}/source`, req), 'Source changed; it applies to the next deployment');
        if (res && rerender) rerender();
      },
    },
    h('div', { class: 'row' },
      h('label', { class: 'small', for: 'source-kind-git' }, kindGit, ' Git repository'),
      h('label', { class: 'small', for: 'source-kind-dir' }, kindDir, ' Folder on this server')),
    gitFields, dirFields,
    h('div', { class: 'field' }, h('label', { for: 'source-subdir' }, 'App folder (optional)'), subIn,
      h('span', { class: 'hint' }, 'Build and run only this folder of the source, e.g. apps/web in a monorepo.')),
    h('div', { class: 'form-actions' }, saveBtn));
    changePanel.replaceChildren(form);
    (git ? repoIn : dirIn).focus();
  });
  return h('section', { class: 'card', id: 'repository', 'data-card': 'repository' },
    cardHeader('Repository', 'app.repository', h('span', { class: 'spacer' }), changeBtn),
    h('div', { class: 'card-body' },
      facts,
      changePanel,
      project.repository_url
        ? h('div', {}, h('h3', {}, 'Deploy key'), deployKeyPanel(project, ctx))
        : h('p', { class: 'muted small' }, 'Deployments copy the directory above, so no deploy key is needed.')));
}

// --- Scan a folder (wizard) ---------------------------------------------

const SCAN_WARNING_KIND = {
  port_in_use: 'bad',
  env_unreadable: 'bad',
  ssh_remote: 'warn',
  private_repo: 'warn',
  hardcoded_port: 'info',
  credentials_in_url: 'warn',
  env_outside_allowed: 'warn',
};

/** The folder form; results (and the import) render into `out`. */
async function scanForm(ctx, panel, out) {
  const info = await api('GET', '/apps/scan');
  if (!ctx.alive()) return;
  if (!info.allowed_dirs.length) {
    replace(panel, h('div', { class: 'alert warn', 'data-scan-off': '' },
      'Scanning is turned off on this server. List the folders it may look in in the daemon configuration (/etc/aegis/aegis.toml), e.g. ',
      h('code', {}, '[apps] import_dirs = ["/opt/github"]'), ', then restart Aegis.'));
    return;
  }
  const pathIn = h('input', { id: 'scan-path', class: 'mono', value: info.allowed_dirs[0], autocomplete: 'off', spellcheck: 'false' });
  const scanBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'scan' }, 'Scan →');
  const doScan = async () => {
    const res = await run(scanBtn, () => api('POST', '/apps/scan', { path: pathIn.value.trim() }));
    if (res && ctx.alive()) drawScanResults(ctx, out, res, info);
  };
  scanBtn.addEventListener('click', doScan);
  pathIn.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      doScan();
    }
  });
  replace(panel,
    h('div', { class: 'field' }, h('label', { for: 'scan-path' }, 'Folder on this server'), pathIn,
      h('span', { class: 'hint' }, 'Allowed: ', info.allowed_dirs.map((d, i) => [i ? ', ' : '', h('code', {}, d)]),
        '. Aegis looks two levels deep for apps (package.json, pyproject.toml, requirements.txt, Cargo.toml, go.mod, pom.xml, build.gradle, Dockerfile, aegis.toml), including monorepo apps in apps/, packages/ and services/. It only reads; .env values are never shown.')),
    h('div', { class: 'form-actions' }, scanBtn));
}

function drawScanResults(ctx, out, res, info) {
  const rows = res.candidates.map((c, i) => {
    const fresh = !c.registered_as;
    const pick = h('input', { type: 'checkbox', checked: fresh, disabled: !fresh, 'aria-label': `Import ${c.name}` });
    const nameIn = h('input', { class: 'mono', value: c.name, 'aria-label': `Name for ${c.path}`, disabled: !fresh });
    const startIn = h('input', { class: 'mono', value: c.start_command, 'aria-label': `Start command for ${c.name}`, disabled: !fresh });
    const portIn = h('input', { type: 'number', min: 1, max: 65535, value: c.port ?? '', 'aria-label': `Port for ${c.name}`, disabled: !fresh, class: 'port-input' });
    const useEnv = h('input', { type: 'checkbox', checked: c.env_file_allowed, disabled: !fresh || !c.env_file_allowed, 'aria-label': `Use ${c.env_file || '.env'} as the Env file` });
    const sourceSel = h('select', { 'aria-label': `Source for ${c.name}`, disabled: !fresh || !c.repository_url },
      c.repository_url ? h('option', { value: 'git' }, 'Clone the repository') : null,
      h('option', { value: 'dir' }, 'Copy this folder'));
    const access = h('div', { class: 'small' });
    const checkBtn = c.remote_kind === 'github' && fresh
      ? h('button', { class: 'btn small', type: 'button', 'data-action': 'check-access' }, 'Check')
      : null;
    if (checkBtn) {
      checkBtn.addEventListener('click', async () => {
        const r = await run(checkBtn, () => api('POST', '/apps/check-access', { repository_url: c.repository_url, branch: c.branch }));
        if (!r) return;
        replace(access, r.ok
          ? pill('Public', 'ok')
          : [pill('Private: deploy key needed', 'warn'), h('div', { class: 'muted' }, r.hint || '')]);
      });
    }
    const warnings = c.warnings.map((w) => h('li', { 'data-warning': w.code, class: SCAN_WARNING_KIND[w.code] === 'bad' ? 'bad' : '' }, w.message));
    const tr = h('tr', { 'data-candidate': c.path, 'data-registered': c.registered_as ? 'yes' : 'no' },
      h('td', {}, pick),
      h('td', { 'data-label': 'Name' }, nameIn, h('div', { class: 'small muted' }, c.runtime)),
      h('td', { 'data-label': 'Source', class: 'small' },
        h('div', { class: 'mono truncate', title: c.path }, c.path),
        c.repository_url ? h('div', { class: 'mono truncate', title: c.repository_url }, `${c.repository_url} @ ${c.branch || 'main'}`) : null,
        c.source_subdir ? h('div', {}, 'App folder: ', h('code', {}, c.source_subdir)) : null,
        c.registered_as ? pill(`Already imported as ${c.registered_as}`, '') : sourceSel,
        checkBtn, access),
      h('td', { 'data-label': 'Start command' }, startIn),
      h('td', { 'data-label': 'Port' }, portIn),
      h('td', { 'data-label': 'Env file', class: 'small' },
        c.env_file
          ? [useEnv, ' ', h('span', { class: 'mono' }, '.env'), c.env_names.length ? h('div', { class: 'muted' }, c.env_names.join(', ')) : null]
          : h('span', { class: 'muted' }, 'none')),
      h('td', { 'data-label': 'Warnings', class: 'small' }, warnings.length ? h('ul', { class: 'scan-warnings' }, warnings) : h('span', { class: 'muted' }, '–')));
    return { c, tr, pick, nameIn, startIn, portIn, useEnv, sourceSel, i };
  });

  const importBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'import-selected' }, 'Import selected');
  const outcome = h('div');
  importBtn.addEventListener('click', async () => {
    const chosen = rows.filter((r) => r.pick.checked);
    if (!chosen.length) {
      toast('Select at least one app', 'bad');
      return;
    }
    const apps = chosen.map(({ c, nameIn, startIn, portIn, useEnv, sourceSel }) => {
      const app = { name: nameIn.value.trim(), runtime: c.runtime };
      if (sourceSel.value === 'git' && c.repository_url) {
        app.repository_url = c.repository_url;
        app.branch = c.branch || 'main';
        if (c.source_subdir) app.source_subdir = c.source_subdir;
      } else {
        app.source_dir = c.path;
      }
      const settings = {};
      const start = startIn.value.trim();
      if (start && (start !== c.start_command || c.start_hint)) settings.start_command = start;
      const port = portIn.value ? Number(portIn.value) : null;
      if (port && (port !== c.port || c.start_hint)) {
        settings.port = port;
        if (c.health_check_url) settings.health_check_url = c.health_check_url.replace(`:${c.port}`, `:${port}`);
      }
      if (useEnv.checked && c.env_file) settings.env_file = c.env_file;
      if (Object.keys(settings).length) app.settings = settings;
      return app;
    });
    const result = await run(importBtn, () => api('POST', '/apps/import', { apps }));
    if (!result || !ctx.alive()) return;
    const ok = result.apps.filter((a) => a.project_id).length;
    toast(`${ok} app${ok === 1 ? '' : 's'} imported (not deployed)`);
    replace(outcome, h('section', { class: 'card', 'data-import-result': '' },
      h('div', { class: 'card-header' }, h('h2', {}, 'Imported — next steps')),
      h('div', { class: 'card-body grid' },
        result.apps.map((a) => h('div', { 'data-imported': a.name },
          a.project_id
            ? h('h3', {}, h('a', { href: `#/apps/${enc(a.name)}/settings` }, a.name), ' ', pill('Imported', 'ok'))
            : h('h3', {}, a.name, ' ', pill('Not imported', 'bad')),
          a.error ? errorBox({ message: a.error }) : null,
          a.next_steps.length ? h('ol', { class: 'update-steps small' }, a.next_steps.map((s) => h('li', {}, s))) : null)))));
    state.projects = await api('GET', '/projects').catch(() => state.projects);
    for (const r of rows) {
      if (result.apps.some((a) => a.project_id && a.name === r.nameIn.value.trim())) {
        r.pick.checked = false;
        r.pick.disabled = true;
      }
    }
  });

  const fresh = rows.filter((r) => !r.c.registered_as).length;
  replace(out, h('section', { class: 'card', 'data-scan-results': '' },
    h('div', { class: 'card-header' }, h('h2', {}, `Found ${res.candidates.length} app${res.candidates.length === 1 ? '' : 's'} in ${res.root}`), helpButton('wizard.scan')),
    h('div', { class: 'card-body' },
      h('p', { class: 'muted small' },
        `${fresh} not imported yet. Imported apps are registered without deploying; each gets its next steps. `,
        info.env_files_enabled ? '' : 'Env files are off ([apps] env_file_dirs), so .env files can only be imported on each app\'s Environment card. '),
      res.truncated ? h('div', { class: 'alert warn' }, 'The scan stopped at its limit; scan a narrower folder to see the rest.') : null,
      res.skipped.length ? h('details', { class: 'small' }, h('summary', {}, `${res.skipped.length} folder(s) skipped`), h('ul', {}, res.skipped.map((s) => h('li', { class: 'mono' }, s)))) : null),
    rows.length
      ? h('div', { class: 'table-wrap' }, h('table', { class: 'stack scan-table' },
        h('thead', {}, h('tr', {}, ['', 'Name', 'Source', 'Start command', 'Port', 'Env file', 'Warnings'].map((t) => h('th', {}, t)))),
        h('tbody', {}, rows.map((r) => r.tr))))
      : h('div', { class: 'card-body' }, h('p', { class: 'muted' }, 'No apps found here.')),
    rows.length ? h('div', { class: 'card-body' }, h('div', { class: 'form-actions' }, importBtn)) : null),
  outcome);
}

// --- Domains (Settings tab) --------------------------------------------

const TLS_LABELS = {
  acme: "Automatic HTTPS (Let's Encrypt)",
  files: 'HTTPS from certificate files',
  off: 'HTTP only',
};

function certPill(domain) {
  const c = domain.cert;
  if (!c) return pill(domain.tls === 'off' ? 'HTTP only' : 'Proxy off', '');
  const until = c.not_after ? ` until ${new Date(c.not_after).toLocaleDateString()}` : '';
  switch (c.state) {
    case 'valid': return pill('HTTPS', 'ok');
    case 'expiring': return pill('Renewing', 'warn');
    case 'expired': return pill('Expired', 'bad');
    case 'pending': return pill('Issuing certificate', 'info live');
    case 'error': return pill('Certificate error', 'bad');
    case 'disabled': return pill('ACME off', 'warn');
    case 'off': return pill('HTTP only', '');
    default: return pill(`${c.state}${until}`, '');
  }
}

/** The Domains card: map domains to this app through the built-in proxy. */
function domainsCard(ctx) {
  const project = ctx.project();
  const base = `/projects/${enc(project.id)}/domains`;
  const hint = h('div');
  const list = h('div', { class: 'table-wrap' });
  const domainInput = h('input', {
    id: 'domain-name', class: 'mono', placeholder: 'app.example.com',
    autocomplete: 'off', spellcheck: 'false', required: true,
  });
  const tlsSelect = h('select', { id: 'domain-tls' },
    Object.entries(TLS_LABELS).map(([value, label]) => h('option', { value }, label)));
  const certInput = h('input', { id: 'domain-cert', class: 'mono', placeholder: '/etc/ssl/app/fullchain.pem' });
  const keyInput = h('input', { id: 'domain-key', class: 'mono', placeholder: '/etc/ssl/app/privkey.pem' });
  const filesRow = h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'domain-cert' }, 'Certificate chain (PEM path on the server)'), certInput),
    h('div', { class: 'field' }, h('label', { for: 'domain-key' }, 'Private key (PEM path on the server)'), keyInput));
  const syncTls = () => { filesRow.hidden = tlsSelect.value !== 'files'; };
  tlsSelect.addEventListener('change', syncTls);
  syncTls();
  const addBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'add-domain' }, 'Add domain');

  const draw = (domains, proxy) => {
    if (!proxy.enabled) {
      replace(hint, h('div', { class: 'alert warn' },
        'The built-in proxy is off, so domains are saved but not served. Set [proxy] enabled = true in aegis.toml and restart the daemon.'));
    } else {
      replace(hint, h('p', { class: 'muted small' },
        `Point each domain's DNS at this server. The proxy listens on ports ${proxy.http_port} (HTTP) and ${proxy.https_port} (HTTPS); deployments switch over without dropping requests.`,
        proxy.acme_enabled ? '' : ' Automatic HTTPS is off ([proxy.acme]).'));
    }
    if (!domains.length) {
      replace(list, h('p', { class: 'muted small' }, 'No domains yet.'));
      return;
    }
    replace(list, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Domain', 'TLS', 'Certificate', ''].map((t) => h('th', {}, t)))),
      h('tbody', {}, domains.map((d) => {
        const removeBtn = h('button', { class: 'btn small danger', type: 'button', 'data-action': 'remove-domain' }, 'Remove');
        removeBtn.addEventListener('click', async () => {
          const ok = await confirmDialog({
            title: `Remove ${d.domain}?`,
            message: 'The app stops being served on this domain right away. Its certificate is kept on the server in case you add the domain again.',
            confirmLabel: 'Remove',
            danger: true,
          });
          if (!ok) return;
          const done = await run(removeBtn, () => api('DELETE', `${base}/${enc(d.domain)}`), `${d.domain} removed`);
          if (done !== undefined) load();
        });
        const scheme = d.tls === 'off' || d.cert?.state !== 'valid' ? 'http' : 'https';
        return h('tr', { 'data-domain': d.domain },
          h('td', { 'data-label': 'Domain', class: 'mono' },
            h('a', { href: `${scheme}://${d.domain}/`, target: '_blank', rel: 'noopener noreferrer' }, d.domain)),
          h('td', { 'data-label': 'TLS' }, TLS_LABELS[d.tls] || d.tls),
          h('td', { 'data-label': 'Certificate', title: d.cert?.message || '' },
            certPill(d),
            d.cert?.not_after ? h('div', { class: 'small muted' }, `Expires ${new Date(d.cert.not_after).toLocaleDateString()}`) : null,
            d.cert?.message && ['error', 'disabled', 'pending'].includes(d.cert.state)
              ? h('div', { class: 'small muted' }, d.cert.message) : null),
          h('td', { class: 'actions' }, removeBtn));
      }))));
  };

  const load = async () => {
    try {
      const [domains, proxy] = await Promise.all([api('GET', base), api('GET', '/proxy')]);
      if (ctx.alive()) draw(domains, proxy);
    } catch (err) {
      if (ctx.alive()) replace(list, errorBox(err));
    }
  };

  const form = h('form', {
    onsubmit: async (e) => {
      e.preventDefault();
      const req = { domain: domainInput.value.trim(), tls: tlsSelect.value };
      if (!req.domain) return;
      if (req.tls === 'files') {
        req.cert_path = certInput.value.trim();
        req.key_path = keyInput.value.trim();
      }
      const added = await run(addBtn, () => api('POST', base, req), (d) => `${d.domain} added`);
      if (!added) return;
      domainInput.value = '';
      load();
    },
  },
  h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'domain-name' }, 'Domain'), domainInput),
    h('div', { class: 'field' }, h('label', { for: 'domain-tls' }, 'TLS'), tlsSelect)),
  filesRow,
  h('div', { class: 'form-actions' }, addBtn));

  onEvents(ctx, (e) => {
    if (e.event_type === '_resync' || (e.event_type.startsWith('Domain') && e.payload?.project_id === project.id)) load();
  });
  // Certificates are issued in the background; refresh their state.
  every(ctx, 15000, load);
  load();

  return h('section', { class: 'card', id: 'domains' },
    cardHeader('Domains', 'app.domains'),
    h('div', { class: 'card-body' }, hint, list, form));
}

// --- Resources tab -----------------------------------------------------

const MIB = 1024 * 1024;

function fmtCores(cores, limit) {
  if (cores === null || cores === undefined) return '–';
  const used = cores < 10 ? cores.toFixed(2) : cores.toFixed(1);
  return limit ? `${used} / ${limit} cores` : `${used} cores`;
}

function fmtUsage(bytes, limit) {
  return limit ? `${fmtBytes(bytes)} / ${fmtBytes(limit)}` : fmtBytes(bytes);
}

function describeLimits(l) {
  if (!l) return '';
  const parts = [
    l.cpu_cores ? `CPU ${l.cpu_cores} cores` : 'CPU unlimited',
    l.memory_bytes ? `memory ${fmtBytes(l.memory_bytes)}` : 'memory unlimited',
  ];
  if (l.pids_max) parts.push(`${l.pids_max} processes`);
  return parts.join(', ');
}

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/**
 * A line chart on a canvas: the value, its peak (faint), and the limit as a
 * dashed line. `format` labels the y axis. Returns {el, update(points)}.
 */
function lineChart(ctx, { label, format, color }) {
  const canvas = h('canvas', { role: 'img', 'aria-label': `${label} chart` });
  const readout = h('div', { class: 'chart-readout muted small', 'aria-live': 'off' }, ' ');
  const wrap = h('div', { class: 'chart' }, canvas);
  let points = [];
  let hover = null;

  const draw = () => {
    const dpr = window.devicePixelRatio || 1;
    const width = wrap.clientWidth;
    const height = wrap.clientHeight;
    if (!width || !height) return;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    const g = canvas.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, width, height);
    const pad = { l: 64, r: 10, t: 10, b: 22 };
    const w = width - pad.l - pad.r;
    const hgt = height - pad.t - pad.b;
    const text = cssVar('--text-muted');
    const grid = cssVar('--border');
    const line = cssVar(color);
    const limitColor = cssVar('--bad');
    g.font = `11px ${cssVar('--font') || 'sans-serif'}`;
    g.fillStyle = text;

    if (!points.length) {
      g.textAlign = 'center';
      g.fillText('Waiting for data…', pad.l + w / 2, pad.t + hgt / 2);
      return;
    }
    const t0 = points[0].at;
    const t1 = Math.max(points[points.length - 1].at, t0 + 1);
    let top = 0;
    for (const p of points) top = Math.max(top, p.max ?? p.value, p.limit ?? 0);
    top = top > 0 ? top * 1.12 : 1;
    const x = (t) => pad.l + ((t - t0) / (t1 - t0)) * w;
    const y = (v) => pad.t + hgt - (v / top) * hgt;

    // Grid and y labels.
    g.strokeStyle = grid;
    g.lineWidth = 1;
    g.textAlign = 'right';
    g.textBaseline = 'middle';
    for (let i = 0; i <= 4; i += 1) {
      const v = (top / 4) * i;
      const yy = Math.round(y(v)) + 0.5;
      g.beginPath();
      g.moveTo(pad.l, yy);
      g.lineTo(pad.l + w, yy);
      g.stroke();
      g.fillText(format(v), pad.l - 6, yy);
    }
    // Time labels.
    g.textBaseline = 'top';
    g.textAlign = 'left';
    g.fillText(new Date(t0).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }), pad.l, pad.t + hgt + 6);
    g.textAlign = 'right';
    g.fillText(new Date(t1).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }), pad.l + w, pad.t + hgt + 6);

    const path = (key) => {
      g.beginPath();
      points.forEach((p, i) => {
        const v = p[key] ?? p.value;
        if (i === 0) g.moveTo(x(p.at), y(v));
        else g.lineTo(x(p.at), y(v));
      });
    };
    // Peak (faint), then area and line for the average.
    if (points.some((p) => p.max !== undefined && p.max !== p.value)) {
      g.globalAlpha = 0.35;
      g.strokeStyle = line;
      path('max');
      g.stroke();
      g.globalAlpha = 1;
    }
    path('value');
    g.lineTo(x(points[points.length - 1].at), y(0));
    g.lineTo(x(points[0].at), y(0));
    g.closePath();
    g.globalAlpha = 0.14;
    g.fillStyle = line;
    g.fill();
    g.globalAlpha = 1;
    g.strokeStyle = line;
    g.lineWidth = 2;
    path('value');
    g.stroke();

    // Limit: a dashed step line wherever one applied.
    g.strokeStyle = limitColor;
    g.lineWidth = 1.5;
    g.setLineDash([6, 4]);
    g.beginPath();
    let open = false;
    for (const p of points) {
      if (p.limit) {
        if (!open) g.moveTo(x(p.at), y(p.limit));
        else g.lineTo(x(p.at), y(p.limit));
        open = true;
      } else if (open) {
        g.stroke();
        g.beginPath();
        open = false;
      }
    }
    if (open) g.stroke();
    g.setLineDash([]);

    if (hover !== null && points[hover]) {
      const p = points[hover];
      g.strokeStyle = text;
      g.lineWidth = 1;
      g.beginPath();
      g.moveTo(Math.round(x(p.at)) + 0.5, pad.t);
      g.lineTo(Math.round(x(p.at)) + 0.5, pad.t + hgt);
      g.stroke();
    }
  };

  const showReadout = () => {
    const p = points[hover ?? points.length - 1];
    readout.textContent = p
      ? `${new Date(p.at).toLocaleTimeString()} · ${format(p.value)}${p.max !== undefined && p.max !== p.value ? ` (peak ${format(p.max)})` : ''}${p.limit ? ` · limit ${format(p.limit)}` : ''}`
      : ' ';
  };

  canvas.addEventListener('mousemove', (e) => {
    if (!points.length) return;
    const rect = canvas.getBoundingClientRect();
    const t0 = points[0].at;
    const t1 = Math.max(points[points.length - 1].at, t0 + 1);
    const t = t0 + ((e.clientX - rect.left - 64) / (rect.width - 74)) * (t1 - t0);
    let best = 0;
    for (let i = 1; i < points.length; i += 1) if (Math.abs(points[i].at - t) < Math.abs(points[best].at - t)) best = i;
    hover = best;
    showReadout();
    draw();
  });
  canvas.addEventListener('mouseleave', () => {
    hover = null;
    showReadout();
    draw();
  });

  const observer = new ResizeObserver(() => draw());
  observer.observe(wrap);
  const redraw = () => draw();
  window.addEventListener('aegis-theme', redraw);
  const media = window.matchMedia('(prefers-color-scheme: dark)');
  media.addEventListener('change', redraw);
  ctx.cleanup(() => {
    observer.disconnect();
    window.removeEventListener('aegis-theme', redraw);
    media.removeEventListener('change', redraw);
  });

  return {
    el: h('div', {}, wrap, readout),
    canvas,
    update(next) {
      points = next;
      showReadout();
      draw();
    },
  };
}

const RANGES = [['5m', 'Live · 5 min'], ['1h', '1 hour'], ['24h', '24 hours'], ['7d', '7 days']];
const RANGE_MS = { '5m': 300_000, '1h': 3_600_000, '24h': 86_400_000, '7d': 604_800_000 };

async function resourcesTab(body, ctx) {
  const project = ctx.project();
  const current = await api('GET', `/projects/${enc(project.id)}/resources`);
  if (!ctx.alive()) return;
  // What this app can be limited by: the host's cgroups, or Docker for
  // container apps.
  const caps = current.capabilities;
  let limits = current.limits;
  let range = '5m';
  let raw = [];

  // --- Live numbers ---------------------------------------------------
  const stat = (key, label) => {
    const value = h('div', { class: 'value' }, '–');
    return { key, value, el: h('div', { class: 'card stat', 'data-stat': key }, h('div', { class: 'label' }, label), value) };
  };
  const stats = [
    stat('cpu', 'CPU'), stat('memory', 'Memory'), stat('throttled', 'CPU throttled'),
    stat('threads', 'Threads'), stat('fds', 'Open files'), stat('oom', 'OOM kills'),
  ];
  const showUsage = (u) => {
    const set = (key, text) => {
      stats.find((x) => x.key === key).value.textContent = text;
    };
    if (!u) {
      for (const x of stats) x.value.textContent = '–';
      return;
    }
    set('cpu', fmtCores(u.cpu_cores, u.cpu_limit_cores));
    set('memory', fmtUsage(u.memory_bytes, u.memory_limit_bytes));
    set('throttled', `${Math.round(u.throttled_ratio * 100)}%`);
    set('threads', `${u.threads} in ${u.procs} process${u.procs === 1 ? '' : 'es'}`);
    set('fds', String(u.fds));
    set('oom', String(u.oom_kills));
  };
  showUsage(current.usage);

  // --- Charts ----------------------------------------------------------
  const cpuChart = lineChart(ctx, { label: 'CPU', color: '--accent', format: (v) => `${v < 1 ? v.toFixed(2) : v.toFixed(1)} cores` });
  const memChart = lineChart(ctx, { label: 'Memory', color: '--ok', format: (v) => fmtBytes(v) });
  const redrawCharts = () => {
    const cutoff = Date.now() - RANGE_MS[range];
    const pts = raw.filter((p) => p.at >= cutoff);
    cpuChart.update(pts.map((p) => ({ at: p.at, value: p.cpu_cores, max: p.cpu_cores_max, limit: p.cpu_limit_cores })));
    memChart.update(pts.map((p) => ({ at: p.at, value: p.memory_bytes, max: p.memory_bytes_max, limit: p.memory_limit_bytes })));
  };
  const loadHistory = async () => {
    const m = await api('GET', `/projects/${enc(project.id)}/metrics?range=${range}&points=400`);
    if (!ctx.alive()) return;
    raw = m.points;
    redrawCharts();
  };
  const rangeButtons = h('div', { class: 'segmented', role: 'group', 'aria-label': 'Time range' },
    RANGES.map(([value, text]) => h('button', {
      class: 'btn small',
      type: 'button',
      'aria-pressed': value === range ? 'true' : 'false',
      'data-range': value,
      onclick: (e) => {
        range = value;
        for (const b of rangeButtons.children) b.setAttribute('aria-pressed', String(b === e.currentTarget));
        loadHistory().catch((err) => toast(err.message, 'bad'));
      },
    }, text)));

  // Live samples: extend the charts while the "Live" range is shown.
  const stream = new EventSource(`/api/v1/metrics/stream?project=${enc(project.id)}`);
  stream.addEventListener('sample', (m) => {
    let update;
    try {
      update = JSON.parse(m.data);
    } catch {
      return;
    }
    const smp = update.sample;
    showUsage(smp);
    if (range !== '5m') return;
    raw.push({
      at: smp.at, cpu_cores: smp.cpu_cores, cpu_cores_max: smp.cpu_cores, memory_bytes: smp.memory_bytes,
      memory_bytes_max: smp.memory_bytes, cpu_limit_cores: smp.cpu_limit_cores, memory_limit_bytes: smp.memory_limit_bytes,
    });
    redrawCharts();
  });
  ctx.cleanup(() => stream.close());

  // --- Limits ------------------------------------------------------------
  const canLimit = caps.cpu || caps.memory;
  const maxMemMib = Math.max(64, Math.floor(caps.host_memory_bytes / MIB));
  const cpuOff = h('input', { type: 'checkbox', id: 'cpu-unlimited', checked: !limits.cpu_cores, disabled: !caps.cpu });
  const cpuRange = h('input', {
    type: 'range', id: 'cpu-limit', min: 0.1, max: caps.host_cpu_cores, step: 0.05,
    value: limits.cpu_cores ?? caps.host_cpu_cores, disabled: !caps.cpu || !limits.cpu_cores, 'aria-label': 'CPU limit in cores',
  });
  const cpuValue = h('output', { for: 'cpu-limit', class: 'slider-value', 'data-value': 'cpu' });
  const memOff = h('input', { type: 'checkbox', id: 'mem-unlimited', checked: !limits.memory_bytes, disabled: !caps.memory });
  const memRange = h('input', {
    type: 'range', id: 'mem-limit', min: 16, max: maxMemMib, step: 16,
    value: limits.memory_bytes ? Math.round(limits.memory_bytes / MIB) : maxMemMib,
    disabled: !caps.memory || !limits.memory_bytes, 'aria-label': 'Memory limit in MiB',
  });
  const memNumber = h('input', {
    type: 'number', id: 'mem-limit-mib', min: 8, max: maxMemMib, step: 1, class: 'slider-number',
    value: memRange.value, disabled: memRange.disabled, 'aria-label': 'Memory limit (MiB)',
  });
  const pidsInput = h('input', {
    type: 'number', id: 'pids-limit', min: 1, placeholder: 'unlimited', value: limits.pids_max ?? '',
    disabled: !caps.pids, class: 'slider-number',
  });
  const applied = h('span', { class: 'muted small', 'data-applied': '' });

  const syncLabels = () => {
    cpuRange.disabled = !caps.cpu || cpuOff.checked;
    memRange.disabled = !caps.memory || memOff.checked;
    memNumber.disabled = memRange.disabled;
    cpuValue.textContent = cpuOff.checked ? 'No limit' : `${Number(cpuRange.value).toFixed(2)} cores`;
    memNumber.value = memRange.value;
  };
  const wanted = () => ({
    cpu_cores: caps.cpu && !cpuOff.checked ? Number(Number(cpuRange.value).toFixed(2)) : null,
    memory_bytes: caps.memory && !memOff.checked ? Number(memNumber.value) * MIB : null,
    pids_max: caps.pids && pidsInput.value ? Number(pidsInput.value) : null,
  });
  const resetInputs = () => {
    cpuOff.checked = !limits.cpu_cores;
    if (limits.cpu_cores) cpuRange.value = limits.cpu_cores;
    memOff.checked = !limits.memory_bytes;
    if (limits.memory_bytes) memRange.value = Math.round(limits.memory_bytes / MIB);
    memNumber.value = limits.memory_bytes ? Math.round(limits.memory_bytes / MIB) : memRange.value;
    pidsInput.value = limits.pids_max ?? '';
    syncLabels();
  };

  let saving = false;
  let again = false;
  const apply = async () => {
    // Keyboard moves fire several changes; apply the last one after the
    // request in flight.
    if (saving) {
      again = true;
      return;
    }
    saving = true;
    const req = wanted();
    applied.textContent = 'Applying…';
    try {
      let res;
      try {
        res = await api('PUT', `/projects/${enc(project.id)}/resources`, req);
      } catch (err) {
        if (err.code !== 'confirmation_required') throw err;
        const ok = await confirmDialog({
          title: 'Apply this limit anyway?',
          message: err.message,
          confirmLabel: 'Apply anyway',
          danger: true,
        });
        if (!ok) {
          resetInputs();
          applied.textContent = 'Not changed.';
          return;
        }
        res = await api('PUT', `/projects/${enc(project.id)}/resources`, { ...req, force: true });
      }
      limits = res.limits;
      applied.textContent = `Applied at ${new Date().toLocaleTimeString()}, without a restart.`;
      toast(`${project.name}: ${describeLimits(limits)}`);
    } catch (err) {
      if (err.status !== 401) toast(err.message, 'bad');
      resetInputs();
      applied.textContent = '';
    } finally {
      saving = false;
      if (again) {
        again = false;
        apply();
      }
    }
  };

  cpuRange.addEventListener('input', syncLabels);
  memRange.addEventListener('input', syncLabels);
  memNumber.addEventListener('input', () => {
    memRange.value = memNumber.value;
    cpuValue.textContent = cpuOff.checked ? 'No limit' : `${Number(cpuRange.value).toFixed(2)} cores`;
  });
  // Moving a slider applies it when released (or on Enter in a number box).
  for (const el of [cpuRange, memRange, memNumber, pidsInput]) el.addEventListener('change', apply);
  for (const box of [cpuOff, memOff]) box.addEventListener('change', () => { syncLabels(); apply(); });
  resetInputs();

  const limitRow = (title, control, hint) => h('div', { class: 'limit' },
    h('div', { class: 'limit-head' }, h('h3', {}, title), hint),
    control);
  const limitsCard = h('section', { class: 'card', 'data-limits': '' },
    cardHeader('Limits', 'app.limits', h('span', { class: 'spacer' }), applied),
    h('div', { class: 'card-body' },
      canLimit
        ? h('p', { class: 'muted small' }, 'Changes apply to the running app immediately, without a restart, and again on every start and deploy. ',
          (project.instances || []).length > 1 ? `Each limit applies to each of the ${project.instances.length} instances on its own; usage above is their total. ` : '',
          caps.reason ? caps.reason : '')
        : h('div', { class: 'alert warn', 'data-unsupported': '' },
          h('strong', {}, 'Limits are not available on this server. '), caps.reason || '',
          ' Usage is still measured.'),
      can('admin') ? null : h('p', { class: 'muted small' }, 'Only admins can change limits.'),
      // A disabled fieldset disables every control in it, whatever their own state.
      h('fieldset', { class: 'plain', disabled: !can('admin') },
        limitRow('CPU', h('div', { class: 'slider' }, cpuRange, cpuValue),
          h('label', { class: 'small', for: 'cpu-unlimited' }, cpuOff, ' No limit')),
        limitRow('Memory', h('div', { class: 'slider' }, memRange, memNumber, h('span', { class: 'muted small' }, 'MiB')),
          h('label', { class: 'small', for: 'mem-unlimited' }, memOff, ' No limit')),
        caps.pids ? limitRow('Processes', h('div', { class: 'slider' }, pidsInput), h('span', { class: 'muted small' }, 'Max processes and threads')) : null),
      h('p', { class: 'muted small' }, `Over the memory limit, the kernel stops the app and Aegis restarts it. The soft limit (90%) makes it reclaim memory first. Host: ${caps.host_cpu_cores} cores, ${fmtBytes(caps.host_memory_bytes)}; apps are promised ${fmtBytes(caps.committed_memory_bytes)} in total.`)));

  const chartCard = (title, chart) => h('section', { class: 'card' },
    h('div', { class: 'card-header' }, h('h2', {}, title)),
    h('div', { class: 'card-body' }, chart.el));

  append(body,
    h('div', { class: 'section-head' }, h('h2', {}, 'Live usage'), helpButton('app.resources')),
    h('div', { class: 'grid stats compact' }, stats.map((x) => x.el)),
    h('div', { class: 'section page-header' }, h('h2', {}, 'Usage'), helpButton('app.usage'), h('div', { class: 'spacer' }), rangeButtons),
    h('div', { class: 'grid two' }, chartCard('CPU', cpuChart), chartCard('Memory', memChart)),
    h('div', { class: 'section' }, limitsCard));
  await loadHistory();
  // Keep longer ranges fresh.
  every(ctx, 60_000, () => {
    if (range !== '5m') loadHistory().catch(() => {});
  });
}

// --- Activity tab ------------------------------------------------------

async function activityTab(body, ctx) {
  const project = ctx.project();
  const list = h('ul', { class: 'events', 'data-events': '' });
  append(body, h('div', { class: 'card' }, cardHeader('Activity', 'app.activity'), list));
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
      bits.push(`${p.version} live${p.previous_version ? ` (was ${p.previous_version})` : ''}${p.process_ids && p.process_ids.length > 1 ? ` · ${p.process_ids.length} instances` : ''}`);
      break;
    case 'RolloutStepCompleted':
      bits.push(`${p.version} step ${p.step}/${p.steps}: started ${instanceList(p.started) || 'nothing'}, stopped ${(p.stopped || []).length}`);
      break;
    case 'RolloutAborted':
      bits.push(`${p.version} stopped at step ${p.step}/${p.steps}: ${p.reason || ''}`);
      break;
    case 'TrafficSwitched':
      bits.push(`${p.from_version || '–'} → ${p.to_version}${p.instant_rollback ? ' (instant rollback)' : ''}`);
      break;
    case 'StandbyKept':
      bits.push(`${p.version} kept until ${fmtTime(p.until)}`);
      break;
    case 'StandbyStopped':
      bits.push(`${p.version} (${p.reason})`);
      break;
    case 'InstanceSetChanged':
      bits.push(`${(p.process_ids || []).length} instances (${p.reason || ''})`);
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
    case 'ResourceLimitsChanged':
      bits.push(describeLimits(p.limits));
      break;
    case 'DomainMapped':
      bits.push(`${p.domain} (${TLS_LABELS[p.tls] || p.tls})`);
      break;
    case 'BackupCreated':
      bits.push(`${p.name} · ${fmtBytes(p.size_bytes)}${p.includes_releases ? '' : ' · without releases'} · ${p.trigger}`);
      break;
    case 'BackupFailed':
      bits.push(`${p.trigger}: ${p.error || ''}`);
      break;
    case 'BackupDeleted':
      bits.push(`${p.name} (${p.reason})`);
      break;
    case 'DomainUnmapped':
      bits.push(p.domain);
      break;
    case 'UpdateAvailable':
      bits.push(`${p.latest_version} (running ${p.current_version})`);
      break;
    case 'UpdateStarted':
    case 'UpdateApplied':
      bits.push(`${p.from_version} → ${p.to_version}${p.mode || p.method ? ` · ${p.mode || p.method}` : ''}`);
      break;
    case 'UpdateFailed':
      bits.push(`${p.from_version} → ${p.to_version} · ${p.stage}: ${p.error || ''}`);
      break;
    case 'ResourcePressureDetected':
      bits.length = 0;
      bits.push(p.message);
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
  if (/Failed|Crashed|ResourcePressure/.test(type)) return 'bad';
  if (/RolledBack|LoginFailed/.test(type)) return 'warn';
  if (/Completed|Succeeded|Promoted|LoggedIn|BackupCreated|UpdateApplied/.test(type)) return 'ok';
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
  ['resources', 'Resources & alerts', (t) => t.startsWith('Resource')],
  ['config', 'App configuration', (t) => /^(Project|Schedule)/.test(t)],
  ['backup', 'Backups', (t) => t.startsWith('Backup')],
  ['update', 'Aegis updates', (t) => t.startsWith('Update')],
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
    pageHeader('Events', 'events', count),
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
  append(main, pageHeader('Deployments', 'deployments'), wrap);
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
    pageHeader(`Deployment ${shortId(d.id)}`, 'deployment', h('a', { class: 'btn', href: `#/apps/${enc(name)}` }, `Open ${name}`)),
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
  append(main, h('a', { class: 'small', href: '#/' }, '← All apps'), pageHeader('New app', 'wizard'), body);

  // Step 1: where the code comes from.
  const kindGit = h('input', { type: 'radio', name: 'kind', value: 'git', checked: true });
  const kindDir = h('input', { type: 'radio', name: 'kind', value: 'dir' });
  const kindScan = h('input', { type: 'radio', name: 'kind', value: 'scan', 'data-kind': 'scan' });
  const repo = h('input', { id: 'wiz-repo', class: 'mono', placeholder: 'https://github.com/you/app.git', autocomplete: 'off' });
  const branch = h('input', { id: 'wiz-branch', class: 'mono', value: 'main' });
  const dir = h('input', { id: 'wiz-dir', class: 'mono', placeholder: '/srv/apps/my-app' });
  const subdir = h('input', { id: 'wiz-subdir', class: 'mono', placeholder: 'apps/web', autocomplete: 'off' });
  const name = h('input', { id: 'wiz-name', placeholder: 'from the repository', autocomplete: 'off' });
  const gitFields = h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'wiz-repo' }, 'Repository URL'), repo,
      h('span', { class: 'hint' }, 'https, ssh or a path readable by the daemon. Private GitHub repository? Aegis offers a deploy key if it can\'t read it.')),
    h('div', { class: 'field' }, h('label', { for: 'wiz-branch' }, 'Branch'), branch));
  const dirFields = h('div', { class: 'field' }, h('label', { for: 'wiz-dir' }, 'Directory on this server'), dir);
  const subdirField = h('div', { class: 'field' }, h('label', { for: 'wiz-subdir' }, 'App folder in the source (optional)'), subdir,
    h('span', { class: 'hint' }, 'For monorepos: only this folder is built and run, e.g. apps/web. Files outside it are not part of the release.'));
  const nameField = h('div', { class: 'field' }, h('label', { for: 'wiz-name' }, 'App name'), name, h('span', { class: 'hint' }, 'Optional; used in URLs and the CLI'));
  const scanPanel = h('div', { 'data-scan-panel': '' });
  const error = h('div');
  const detectBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'detect' }, 'Detect settings →');
  const detectActions = h('div', { class: 'form-actions' }, detectBtn);
  const scanResults = h('div');
  let scanLoaded = false;
  const syncKind = () => {
    const scan = kindScan.checked;
    gitFields.hidden = !kindGit.checked;
    dirFields.hidden = !kindDir.checked;
    subdirField.hidden = scan;
    nameField.hidden = scan;
    detectActions.hidden = scan;
    scanPanel.hidden = !scan;
    scanResults.hidden = !scan;
    if (scan && !scanLoaded) {
      scanLoaded = true;
      scanForm(ctx, scanPanel, scanResults).catch((err) => replace(scanPanel, errorBox(err)));
    }
  };
  kindGit.addEventListener('change', syncKind);
  kindDir.addEventListener('change', syncKind);
  kindScan.addEventListener('change', syncKind);
  syncKind();

  /** After a failed detect of a GitHub URL: is it private? Offer a deploy key. */
  const offerDeployKey = async (source) => {
    let check;
    try {
      check = await api('POST', '/apps/check-access', { repository_url: source.repository_url, branch: source.branch });
    } catch {
      return; // not a GitHub URL
    }
    if (!ctx.alive() || check.ok) return;
    const setupBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'setup-deploy-key' }, 'Create the app and set up a deploy key');
    setupBtn.addEventListener('click', async () => {
      const appName = name.value.trim() || repoName(source.repository_url);
      const created = await run(setupBtn, () => api('POST', '/projects', { name: appName, ...source, ...(subdir.value.trim() ? { source_subdir: subdir.value.trim() } : {}) }));
      if (!created || !ctx.alive()) return;
      showKeyStep(created, source);
    });
    append(error, h('div', { class: 'alert warn', 'data-private-repo': '' },
      h('p', {}, 'Aegis can\'t read this repository without credentials: it is private (or doesn\'t exist). Private repositories are cloned with a deploy key: a key pair made for this app whose public half you add to the repository, read-only.'),
      h('div', { class: 'form-actions' }, setupBtn)));
  };

  const step1 = h('form', {
    class: 'card',
    onsubmit: async (e) => {
      e.preventDefault();
      if (kindScan.checked) return;
      replace(error);
      const source = kindGit.checked
        ? { repository_url: repo.value.trim(), branch: branch.value.trim() || 'main' }
        : { source_dir: dir.value.trim() };
      if (!source.repository_url && !source.source_dir) {
        append(error, errorBox({ message: kindGit.checked ? 'Enter a repository URL' : 'Enter a directory' }));
        return;
      }
      if (subdir.value.trim()) source.source_subdir = subdir.value.trim();
      detectBtn.textContent = 'Detecting…';
      try {
        detectBtn.disabled = true;
        const detected = await api('POST', '/detect', source);
        if (ctx.alive()) showStep2(source, detected);
      } catch (err) {
        append(error, errorBox(err));
        if (source.repository_url) offerDeployKey(source);
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
        h('label', {}, kindDir, 'Directory on this server'),
        h('label', {}, kindScan, 'Scan a folder on the server'))),
    gitFields,
    dirFields,
    subdirField,
    nameField,
    scanPanel,
    detectActions));
  append(body, step1, scanResults);

  // A private repository: the app exists; add its deploy key, then detect.
  const showKeyStep = (created, source) => {
    const err = h('div');
    const nextBtn = h('button', { class: 'btn primary', type: 'button', 'data-action': 'detect-with-key' }, 'Detect settings →');
    nextBtn.addEventListener('click', async () => {
      replace(err);
      const detected = await run(nextBtn, () => api('POST', '/detect', { ...source, project: created.id }));
      if (detected && ctx.alive()) showStep2(source, detected, created);
    });
    const card = h('section', { class: 'card' },
      h('div', { class: 'card-header' }, h('h2', {}, `1b. Deploy key for ${created.name}`), helpButton('app.repository')),
      h('div', { class: 'card-body' },
        h('p', { class: 'muted small' }, `${created.name} was created (nothing is deployed yet). Generate its deploy key, add it to the repository, test access, then continue.`),
        deployKeyPanel(created, ctx),
        err,
        h('div', { class: 'form-actions' }, nextBtn)));
    replace(body, card);
  };

  const showStep2 = (source, detected, existing = null) => {
    const appName = h('input', { id: 'wiz-app-name', required: true, value: existing ? existing.name : (name.value.trim() || detected.name), disabled: !!existing });
    const { inputs, layout } = settingsInputs(detected);
    const hint = detected.start_hint;
    if (hint) {
      // The start script fixes the port: run the same command with $PORT.
      inputs.start_command.value = hint.suggested_start;
      if (detected.port && detected.health_check_url) {
        inputs.health_check_url.value = detected.health_check_url.replace(`:${detected.port}`, `:${hint.port}`);
      }
      inputs.port.value = hint.port;
    }
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
          const created = existing || await api('POST', '/projects', {
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
      hint ? h('div', { class: 'alert info', 'data-start-hint': '' },
        `The start script (${hint.script}) fixes port ${hint.port}. Aegis tells each release its port in $PORT (behind the proxy every release gets its own), so the start command below runs the same thing with $PORT, and the port is set to ${hint.port}.`) : null,
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
  cardHeader('Password', 'account.password'),
  h('div', { class: 'card-body' },
    h('p', { class: 'muted small' }, `Signed in as ${session.name} (${session.role || 'viewer'}). Changing the password signs out every session.`),
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
    cardHeader('API tokens', 'account.tokens'),
    h('div', { class: 'card-body' }, h('p', { class: 'muted small' }, 'For the CLI and CI: send as ', h('code', {}, 'Authorization: Bearer <token>'), '.')),
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Name', 'Scope', 'Created', 'Last used', ''].map((t) => h('th', {}, t)))),
      tokenBody)),
    tokenForm);

  // Backups.
  const backupBody = h('tbody');
  const withReleases = h('input', { id: 'backup-releases', type: 'checkbox', checked: true });
  const restoreHelp = h('div');
  const backupBtn = h('button', {
    class: 'btn primary',
    type: 'button',
    'data-action': 'backup-now',
    onclick: async () => {
      const res = await run(backupBtn, () => api('POST', '/backups', { include_releases: withReleases.checked }),
        (b) => `Backup created (${fmtBytes(b.size_bytes)})`);
      if (res) loadBackups().catch(() => {});
    },
  }, 'Back up now');
  const restoreSteps = (path) => [
    'sudo systemctl stop aegis',
    `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin restore ${path}`,
    'sudo systemctl start aegis',
  ].join('\n');
  const loadBackups = async () => {
    const backups = await api('GET', '/backups');
    if (!ctx.alive()) return;
    replace(backupBody, backups.length ? backups.map((b) => h('tr', { 'data-backup': b.name },
      h('td', { 'data-label': 'Created' }, timeEl(b.created_at, `${fmtTime(b.created_at)} (${fmtAgo(b.created_at)})`)),
      h('td', { 'data-label': 'Contents' }, b.problem
        ? pill('unreadable', 'bad')
        : h('span', { class: 'tag' }, b.includes_releases ? 'database, keys, releases' : 'database and keys')),
      h('td', { 'data-label': 'Size' }, fmtBytes(b.size_bytes)),
      h('td', { class: 'actions' },
        h('a', { class: 'btn small', href: `/api/v1/backups/${enc(b.name)}/download`, download: b.name, 'data-action': 'download' }, 'Download'),
        ' ',
        h('button', {
          class: 'btn small danger',
          type: 'button',
          'data-action': 'delete',
          onclick: async (e) => {
            const ok = await confirmDialog({
              title: 'Delete this backup?',
              message: `${b.name} is removed from the server. Downloaded copies are not affected.`,
              confirmLabel: 'Delete',
              danger: true,
            });
            if (!ok) return;
            await run(e.currentTarget, () => api('DELETE', `/backups/${enc(b.name)}`), 'Backup deleted');
            loadBackups().catch(() => {});
          },
        }, 'Delete'))))
      : h('tr', {}, h('td', { colspan: 4, class: 'empty' }, 'No backups yet.')));
    const steps = restoreSteps(backups[0] ? backups[0].path : '/path/to/aegis-backup-….tar.gz');
    replace(restoreHelp,
      h('p', { class: 'muted small' }, 'Restoring is done on the server with the daemon stopped. The archive is checked first; the current database, key and releases are moved to a ',
        h('code', {}, 'pre-restore-…'), ' folder in the data directory, not deleted.'),
      h('pre', { class: 'restore-steps' }, h('code', {}, steps)),
      h('div', { class: 'form-actions' }, copyButton(steps)));
  };
  const backupCard = h('section', { class: 'card', id: 'backups' },
    cardHeader('Backups', 'account.backups'),
    h('div', { class: 'card-body' },
      h('p', { class: 'muted small' }, 'A backup holds the database, the secrets key (without it, app secrets cannot be read), certificates and, optionally, the release directories. Keep downloads somewhere safe: they contain password hashes and the key. Scheduled backups are set with ',
        h('code', {}, '[backup]'), ' in the daemon configuration.'),
      h('div', { class: 'row' },
        h('label', { class: 'small', for: 'backup-releases' }, withReleases, ' Include releases (can be large)'),
        h('div', { class: 'form-actions' }, backupBtn))),
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['Created', 'Contents', 'Size', ''].map((t) => h('th', {}, t)))),
      backupBody)),
    h('div', { class: 'card-body' }, h('h3', {}, 'Restore'), restoreHelp));

  // Appearance.
  const themeSel = h('select', { id: 'theme', 'aria-label': 'Theme' },
    [['system', 'Match the system'], ['light', 'Light'], ['dark', 'Dark']].map(([v, l]) => h('option', { value: v, selected: storedTheme() === v }, l)));
  themeSel.addEventListener('change', () => applyTheme(themeSel.value));
  const prefs = h('section', { class: 'card' },
    cardHeader('Appearance & shortcuts', 'account.appearance'),
    h('div', { class: 'card-body' },
      h('div', { class: 'field' }, h('label', { for: 'theme' }, 'Theme'), themeSel),
      shortcutList(),
      h('div', { class: 'form-actions' },
        h('button', { class: 'btn', type: 'button', 'data-action': 'restart-tour', onclick: () => startTour() }, 'Take the tour again'))));

  const admin = can('admin');
  append(main, pageHeader('Account', 'account', h('button', { class: 'btn', type: 'button', onclick: signOut }, 'Sign out')),
    h('div', { class: 'grid' },
      admin ? usersCard(ctx, session.name) : null,
      admin ? tokenCard : null,
      admin ? backupCard : null,
      h('div', { class: 'grid two' }, pwForm, prefs)));
  if (admin) await Promise.all([loadTokens(), loadBackups()]);
}

const ROLE_HELP = {
  viewer: 'Viewer: read only',
  deployer: 'Deployer: also deploy, roll back, start and stop',
  admin: 'Admin: everything, including settings, secrets and users',
};

/** 32 hex characters from the browser's CSPRNG. */
function randomPassword() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

function roleSelect(id, value, label) {
  return h('select', { id, 'aria-label': label },
    Object.entries(ROLE_HELP).map(([v, text]) => h('option', { value: v, selected: v === value }, text)));
}

/**
 * Which apps an account may see: "all apps" or a checked list. `read()`
 * gives the request fields (`all_apps` / `apps`).
 */
function appAccessPicker(apps, initial) {
  const all = h('input', { type: 'radio', name: `access-${Math.random().toString(36).slice(2)}`, value: 'all', checked: !initial || initial.all_apps });
  const some = h('input', { type: 'radio', name: all.name, value: 'some', checked: !!initial && !initial.all_apps });
  const boxes = apps.map((a) => ({
    name: a.name,
    box: h('input', { type: 'checkbox', value: a.name, checked: !!initial && !initial.all_apps && (initial.apps || []).includes(a.name), 'aria-label': `Can see ${a.name}` }),
  }));
  const list = h('div', { class: 'app-access-list', hidden: all.checked },
    boxes.length ? boxes.map((b) => h('label', { class: 'small' }, b.box, ' ', b.name)) : h('span', { class: 'muted small' }, 'No apps yet.'));
  const sync = () => { list.hidden = all.checked; };
  all.addEventListener('change', sync);
  some.addEventListener('change', sync);
  return {
    el: h('div', { class: 'field app-access' },
      h('label', { class: 'small' }, all, ' All apps (including apps added later)'),
      h('label', { class: 'small' }, some, ' Only these apps'),
      list),
    read: () => (all.checked ? { all_apps: true } : { all_apps: false, apps: boxes.filter((b) => b.box.checked).map((b) => b.name) }),
  };
}

/** Admins: create users, change roles, reset passwords, delete users. */
function usersCard(ctx, me) {
  const tbody = h('tbody');
  const shown = h('div');
  const showPassword = (username, password) => replace(shown, h('div', { class: 'grid' },
    h('div', { class: 'alert warn' }, `Give ${username} this password; it is not shown again. They can change it after signing in.`),
    secretBox(password, `Password for ${username}`)));

  const load = async () => {
    const users = await api('GET', '/users');
    if (!ctx.alive()) return;
    replace(tbody, users.map((u) => {
      const sel = roleSelect(`role-${u.username}`, u.role, `Role of ${u.username}`);
      sel.addEventListener('change', async () => {
        const done = await run(sel, () => api('PUT', `/users/${enc(u.username)}`, { role: sel.value }), `${u.username} is now ${sel.value}`);
        if (!done) sel.value = u.role;
        load().catch(() => {});
      });
      const resetBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'reset-password' }, 'Reset password');
      resetBtn.addEventListener('click', async () => {
        const ok = await confirmDialog({
          title: `Reset ${u.username}'s password?`,
          message: 'They are signed out everywhere and need the new password, which is shown once.',
          confirmLabel: 'Reset password',
        });
        if (!ok) return;
        const password = randomPassword();
        const done = await run(resetBtn, () => api('PUT', `/users/${enc(u.username)}`, { password }));
        if (done) showPassword(u.username, password);
      });
      const deleteBtn = h('button', { class: 'btn small danger', type: 'button', 'data-action': 'delete-user', disabled: u.username === me }, 'Delete');
      deleteBtn.addEventListener('click', async () => {
        const ok = await confirmDialog({
          title: `Delete ${u.username}?`,
          message: 'They are signed out and can no longer sign in. Their actions stay in the audit log.',
          confirmLabel: 'Delete',
          danger: true,
        });
        if (!ok) return;
        const done = await run(deleteBtn, () => api('DELETE', `/users/${enc(u.username)}`), `${u.username} deleted`);
        if (done !== undefined) load().catch(() => {});
      });
      const appsBtn = h('button', { class: 'btn small', type: 'button', 'data-action': 'user-apps', disabled: u.role === 'admin', title: u.role === 'admin' ? 'Admins always see every app' : '' },
        u.all_apps ? 'All apps' : `${u.apps.length} app${u.apps.length === 1 ? '' : 's'}`);
      appsBtn.addEventListener('click', async () => {
        let apps;
        try {
          apps = await api('GET', '/projects');
        } catch (err) {
          toast(err.message, 'bad');
          return;
        }
        let fields = null;
        const result = await modal(() => {
          const picker = appAccessPicker(apps, u);
          return h('form', { method: 'dialog', onsubmit: () => { fields = picker.read(); } },
            h('div', { class: 'dialog-body' },
              h('h2', {}, `Apps ${u.username} can see`),
              h('p', { class: 'muted' }, 'Other apps are left out of every list and answer "not found". Apps added later are not included until you add them here. Apps hidden behind a password only show to sessions that have unlocked them.'),
              picker.el),
            h('div', { class: 'form-actions' },
              h('button', { class: 'btn', value: 'cancel', type: 'submit', formnovalidate: true }, 'Cancel'),
              h('button', { class: 'btn primary', value: 'ok', type: 'submit', 'data-confirm': '' }, 'Save')));
        });
        if (result !== 'ok' || !fields) return;
        const done = await run(appsBtn, () => api('PUT', `/users/${enc(u.username)}`, fields), `${u.username}: apps saved`);
        if (done !== undefined) load().catch(() => {});
      });
      return h('tr', { 'data-user': u.username },
        h('td', { 'data-label': 'User', class: 'mono' }, u.username, u.username === me ? h('span', { class: 'muted small' }, ' (you)') : null),
        h('td', { 'data-label': 'Role' }, sel),
        h('td', { 'data-label': 'Apps', title: u.all_apps ? '' : u.apps.join(', ') }, appsBtn),
        h('td', { 'data-label': 'Created' }, timeEl(u.created_at)),
        h('td', { class: 'actions' }, resetBtn, ' ', deleteBtn));
    }));
  };

  const nameIn = h('input', { id: 'user-name', class: 'mono', required: true, autocomplete: 'off', placeholder: 'ana', pattern: '[a-z0-9][a-z0-9._\\-]{0,63}' });
  const roleIn = roleSelect('user-role', 'viewer', 'Role');
  // Apps are listed once the form is drawn; admins always see every app.
  let picker = appAccessPicker([], null);
  const accessSlot = h('div', {}, picker.el);
  api('GET', '/projects').then((apps) => {
    if (!ctx.alive()) return;
    picker = appAccessPicker(apps, null);
    replace(accessSlot, picker.el);
  }).catch(() => {});
  roleIn.addEventListener('change', () => { accessSlot.hidden = roleIn.value === 'admin'; });
  const addBtn = h('button', { class: 'btn primary', type: 'submit', 'data-action': 'create-user' }, 'Add user');
  const form = h('form', {
    class: 'card-body',
    onsubmit: async (e) => {
      e.preventDefault();
      const username = nameIn.value.trim();
      const password = randomPassword();
      const access = roleIn.value === 'admin' ? { all_apps: true } : picker.read();
      const created = await run(addBtn, () => api('POST', '/users', { username, password, role: roleIn.value, ...access }), `${username} added`);
      if (!created) return;
      nameIn.value = '';
      showPassword(username, password);
      load().catch(() => {});
    },
  },
  h('div', { class: 'row' },
    h('div', { class: 'field' }, h('label', { for: 'user-name' }, 'Username'), nameIn,
      h('span', { class: 'hint' }, 'Lowercase letters, digits, . _ -')),
    h('div', { class: 'field' }, h('label', { for: 'user-role' }, 'Role'), roleIn)),
  h('div', { class: 'field' }, h('label', {}, 'Apps they can see'), accessSlot),
  h('div', { class: 'form-actions' }, addBtn),
  shown);

  load().catch((err) => replace(tbody, h('tr', {}, h('td', { colspan: 5 }, errorBox(err)))));
  return h('section', { class: 'card', id: 'users' },
    cardHeader('Users', 'account.users'),
    h('div', { class: 'card-body' }, h('p', { class: 'muted small' }, 'Everyone signs in with their own account, so the audit log shows who did what. New users get a random password to pass on.')),
    h('div', { class: 'table-wrap' }, h('table', { class: 'stack' },
      h('thead', {}, h('tr', {}, ['User', 'Role', 'Apps', 'Created', ''].map((t) => h('th', {}, t)))),
      tbody)),
    form);
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
  ['1 – 6', 'Switch tabs on an app page'],
  ['/', 'Search (logs and events)'],
  ['t', 'Toggle light/dark theme'],
  ['?', 'Help, shortcuts and the tour'],
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
  if (document.querySelector('dialog[open]') || tour.active) return;
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
      if (can('admin')) go('/apps/new');
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
      showHelpDialog();
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
