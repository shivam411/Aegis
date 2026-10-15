// Shared test helpers. Every test fails if the page logs a content security
// policy violation or throws an uncaught error.
const base = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const WORK = path.join(__dirname, '..', '.work');

function env() {
  return JSON.parse(fs.readFileSync(path.join(WORK, 'env.json'), 'utf8'));
}

const test = base.test.extend({
  page: async ({ page }, use) => {
    const problems = [];
    page.on('pageerror', (err) => problems.push(`page error: ${err.message}`));
    page.on('console', (msg) => {
      if (msg.type() === 'error' && /Content Security Policy|Refused to/i.test(msg.text())) {
        problems.push(`CSP: ${msg.text()}`);
      }
    });
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (e) => {
        console.error(`Refused to load: ${e.violatedDirective} ${e.blockedURI}`);
      });
    });
    await use(page);
    base.expect(problems, problems.join('\n')).toEqual([]);
  },
});

async function login(page, password = env().password) {
  await page.goto('/');
  await page.getByLabel('Password').fill(password);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await base.expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
}

/** Accepts the confirmation dialog the next action opens. */
async function confirm(page, label) {
  const dialog = page.locator('dialog[open]');
  await base.expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: label }).click();
  await base.expect(dialog).toHaveCount(0);
}

async function appBody(port) {
  try {
    const res = await fetch(`http://127.0.0.1:${port}/`);
    return await res.json();
  } catch {
    return null;
  }
}

function commit(repo, version) {
  const index = path.join(repo, 'index.js');
  fs.writeFileSync(index, fs.readFileSync(index, 'utf8').replace(/version: '[^']*'/, `version: '${version}'`));
  const gitEnv = {
    ...process.env,
    GIT_AUTHOR_NAME: 'e2e',
    GIT_AUTHOR_EMAIL: 'e2e@example.com',
    GIT_COMMITTER_NAME: 'e2e',
    GIT_COMMITTER_EMAIL: 'e2e@example.com',
  };
  execFileSync('git', ['commit', '-qam', version], { cwd: repo, env: gitEnv });
}

module.exports = { test, expect: base.expect, env, login, confirm, appBody, commit };
