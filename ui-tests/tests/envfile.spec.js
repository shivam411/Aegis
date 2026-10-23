// The Settings tab's Environment card: an env file on the server (status,
// variable names and where each value comes from) and importing a .env
// into Variables and Secrets. Values must never reach the page.
const { test, expect, env, login } = require('./fixtures');
const fs = require('fs');

test.describe.configure({ mode: 'serial' });

const SENTINEL = 's3ntinel';

/** Calls the API from the signed-in page. */
async function apiCall(page, method, path, body) {
  return page.evaluate(async ({ method, path, body }) => {
    const { csrf_token: csrf } = await (await fetch('/api/v1/auth/session')).json();
    const res = await fetch(`/api/v1${path}`, {
      method,
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { status: res.status, text: await res.text() };
  }, { method, path, body });
}

test('env file status and .env import', async ({ page }) => {
  const { envApp, envFile } = env();
  await login(page);
  const created = await apiCall(page, 'POST', '/projects', { name: 'envapp', source_dir: envApp });
  expect(created.status, created.text).toBe(201);
  await page.goto('/#/apps/envapp/settings');
  const card = page.locator('[data-card="environment"]');

  await test.step('point the app at its env file', async () => {
    await expect(card).toContainText('Allowed directories');
    await card.getByLabel('Env file path').fill(envFile);
    await card.getByRole('button', { name: 'Save env file' }).click();
    await expect(page.locator('.toast', { hasText: 'Env file saved' })).toBeVisible();
    await expect(card.locator('[data-env-file-status="ok"]')).toContainText('Readable');
    // Names and sources only; PORT from the file is ignored.
    await expect(card.locator('[data-env-var="MONGODB_URI"]')).toContainText('Env file');
    await expect(card.locator('[data-env-var="NEXT_PUBLIC_SITE_URL"]')).toContainText('yes');
    await expect(card.locator('[data-env-var="PORT"]')).toContainText('Set by Aegis');
    await expect(card).toContainText('Lowest to highest');
    expect(await page.content()).not.toContain(SENTINEL);
  });

  await test.step('an unreadable file says why', async () => {
    fs.writeFileSync(envFile, `MONGODB_URI="${SENTINEL}\n`);
    await page.reload();
    const status = page.locator('[data-card="environment"] [data-env-file-status="error"]');
    await expect(status).toContainText('Not readable');
    await expect(status).toContainText('line 1');
    expect(await page.content()).not.toContain(SENTINEL);
    fs.writeFileSync(envFile, `MONGODB_URI=mongodb+srv://app:e2e-${SENTINEL}@cluster0.example.net/img\nNEXT_PUBLIC_SITE_URL=https://img.example.com\nPORT=3000\n`);
  });

  await test.step('import a .env with suggested types', async () => {
    await page.reload();
    await page.getByRole('button', { name: 'Import .env…' }).click();
    const dialog = page.locator('dialog[open]');
    await dialog.getByLabel('Paste a .env file').fill([
      `NEXTAUTH_SECRET=${SENTINEL}-auth`,
      'NEXT_PUBLIC_SITE_URL=https://img.example.com',
      `GCP_PRIVATE_KEY="-----BEGIN PRIVATE KEY-----\\n${SENTINEL}\\n-----END PRIVATE KEY-----\\n"`,
      'NODE_ENV=production',
      'PORT=3000',
    ].join('\n'));
    await dialog.getByRole('button', { name: 'Preview' }).click();
    const row = (name) => dialog.locator(`[data-import-row="${name}"]`);
    await expect(row('NEXTAUTH_SECRET').getByRole('combobox')).toHaveValue('secret');
    await expect(row('NEXT_PUBLIC_SITE_URL').getByRole('combobox')).toHaveValue('variable');
    await expect(row('GCP_PRIVATE_KEY').getByRole('combobox')).toHaveValue('secret');
    await expect(row('NODE_ENV').getByRole('combobox')).toHaveValue('variable');
    await expect(row('PORT').getByRole('combobox')).toHaveValue('skip');
    // Override a suggestion.
    await row('NODE_ENV').getByRole('combobox').selectOption('skip');
    await dialog.getByRole('button', { name: 'Import', exact: true }).click();
    await expect(page.locator('dialog[open]')).toHaveCount(0);
    await expect(page.locator('.toast', { hasText: 'Imported 1 variable(s) and 2 secret(s)' })).toBeVisible();
  });

  await test.step('imported names show with their sources', async () => {
    const envCard = page.locator('[data-card="environment"]');
    await expect(envCard.locator('[data-env-var="NEXTAUTH_SECRET"]')).toContainText('Secret');
    await expect(envCard.locator('[data-env-var="GCP_PRIVATE_KEY"]')).toContainText('no');
    // The Variable now overrides the env file.
    const site = envCard.locator('[data-env-var="NEXT_PUBLIC_SITE_URL"]');
    await expect(site).toContainText('Variable');
    await expect(site).toContainText('Env file');
    await expect(envCard.locator('[data-env-var="NODE_ENV"]')).toHaveCount(0);
    // The Secrets card lists the new secrets.
    await expect(page.getByRole('cell', { name: 'GCP_PRIVATE_KEY', exact: true }).first()).toBeVisible();
    expect(await page.content()).not.toContain(SENTINEL);
    const events = await apiCall(page, 'GET', '/events?project=envapp&limit=500');
    expect(events.text).not.toContain(SENTINEL);
  });

  await test.step('viewers see names, not values, and no controls', async () => {
    const password = `env-viewer-${Date.now()}`;
    const res = await apiCall(page, 'POST', '/users', { username: 'env-viewer', password, role: 'viewer' });
    expect([200, 201]).toContain(res.status);
    await page.getByRole('button', { name: 'Sign out' }).click();
    await login(page, password, { username: 'env-viewer' });
    await page.goto('/#/apps/envapp/settings');
    const envCard = page.locator('[data-card="environment"]');
    await expect(envCard.locator('[data-env-var="MONGODB_URI"]')).toBeVisible();
    await expect(envCard.getByLabel('Env file path')).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Import .env…' })).toHaveCount(0);
    expect(await page.content()).not.toContain(SENTINEL);
  });
});
