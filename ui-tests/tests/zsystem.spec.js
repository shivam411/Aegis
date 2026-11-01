// System page (updates, storage) and the Docker page with its separate permission.
const { test, expect, login } = require('./fixtures');

test.describe.configure({ mode: 'serial' });

test('system page shows updates and storage, and cleanup settings save', async ({ page }) => {
  await login(page);
  await page.getByRole('link', { name: 'System' }).click();
  await expect(page.getByRole('heading', { name: 'System' })).toBeVisible();
  await expect(page.locator('[data-card="updates"]')).toContainText('Running');
  await expect(page.locator('[data-card="storage"] [data-storage-total]')).toBeVisible();
  await expect(page.locator('[data-card="storage"] [data-cat="old_builds"]')).toBeVisible();

  await page.locator('#log-days').fill('9');
  await page.locator('#auto-clean').check();
  await page.locator('[data-action="save-auto"]').click();
  await expect(page.locator('.toast', { hasText: 'Saved' })).toBeVisible();
  await expect(page.locator('#log-days')).toHaveValue('9');
  await expect(page.locator('#auto-clean')).toBeChecked();
  // Put it back.
  await page.locator('#auto-clean').uncheck();
  await page.locator('[data-action="save-auto"]').click();
  await expect(page.locator('#auto-clean')).not.toBeChecked();
});

test('docker is for the owner; others only when allowed', async ({ page }) => {
  await login(page);
  await expect(page.getByRole('link', { name: 'Docker' })).toBeVisible();
  await page.getByRole('link', { name: 'Docker' }).click();
  await expect(page.getByRole('heading', { name: 'Docker' })).toBeVisible();
  // This machine may or may not run Docker: either answer is a proper page.
  await expect(page.locator('[data-card="containers"]')).toContainText(/Containers/);

  await page.goto('/#/account');
  const row = page.locator('tr[data-user="admin"]');
  await expect(row.locator('[data-docker-access="admin"]')).toBeChecked();
  await expect(row.locator('[data-docker-access="admin"]')).toBeDisabled();
});

test('log filters: level chips and regex search', async ({ page }) => {
  await login(page);
  const card = page.locator('[data-app]').first();
  const name = await card.getAttribute('data-app');
  await page.goto(`/#/apps/${name}/logs`);
  await expect(page.locator('[data-log-count]')).toContainText('lines');
  await page.locator('[data-level="debug"]').click();
  await expect(page.locator('[data-level="debug"]')).toHaveAttribute('aria-pressed', 'false');
  await page.locator('#log-regex').check();
  await page.getByLabel('Search logs').fill('(');
  await expect(page.getByLabel('Search logs')).toHaveAttribute('aria-invalid', 'true');
  await page.getByLabel('Search logs').fill('run+ning');
  await expect(page.getByLabel('Search logs')).not.toHaveAttribute('aria-invalid', 'true');
  await page.locator('[data-log-range]').selectOption('custom');
  await expect(page.locator('[data-log-from]')).toBeVisible();
  await page.locator('[data-log-range]').selectOption('3600');
  await expect(page.locator('[data-log-from]')).toBeHidden();
});
