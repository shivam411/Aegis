// Quick-add a subdomain, and the live URL check settings. Uses whichever
// app the earlier specs left; restores what it changes.
const { test, expect, login, confirm } = require('./fixtures');

test.describe.configure({ mode: 'serial' });

test('add a subdomain with a label and a base domain', async ({ page }) => {
  await login(page);
  const name = await page.locator('[data-app]').first().getAttribute('data-app');
  await page.goto(`/#/apps/${name}/settings`);
  const row = page.locator('[data-card="subdomain"]');
  await expect(row).toBeVisible();
  await row.locator('#subdomain-base').selectOption('__other');
  await row.locator('#subdomain-custom').fill('example.test');
  await row.locator('#subdomain-label').fill('api');
  await expect(row.locator('[data-subdomain-preview]')).toHaveText('→ api.example.test');
  await row.locator('[data-action="add-subdomain"]').click();
  const added = page.locator('tr[data-domain="api.example.test"]');
  await expect(added).toBeVisible();
  await added.locator('[data-action="remove-domain"]').click();
  await confirm(page, 'Remove');
  await expect(added).toHaveCount(0);
});

test('the live URL check settings are saved', async ({ page }) => {
  await login(page);
  const name = await page.locator('[data-app]').first().getAttribute('data-app');
  await page.goto(`/#/apps/${name}/settings`);
  const card = page.locator('[data-card="live-check"]');
  await card.locator('#set-verify-url').fill('https://app.example.test/');
  await card.locator('#set-verify-expect').fill('200-399');
  await card.locator('#set-verify-timeout').fill('20');
  await page.getByRole('button', { name: 'Save settings' }).first().click();
  // The save shows the aegis.toml diff first.
  const dlg = page.locator('dialog[open]');
  await expect(dlg).toContainText('verify_url');
  await dlg.getByRole('button', { name: 'Save settings' }).click();
  await expect(page.locator('.toast', { hasText: 'Settings saved' })).toBeVisible();
  await page.reload();
  await expect(page.locator('#set-verify-url')).toHaveValue('https://app.example.test/');

  await test.step('clear it again', async () => {
    await page.locator('#set-verify-url').fill('');
    await page.locator('#set-verify-expect').fill('');
    await page.locator('#set-verify-timeout').fill('');
    await page.getByRole('button', { name: 'Save settings' }).first().click();
    await page.locator('dialog[open]').getByRole('button', { name: 'Save settings' }).click();
    await expect(page.locator('.toast', { hasText: 'Settings saved' }).last()).toBeVisible();
  });
});

test('turn Prometheus metrics on and download the Grafana dashboard', async ({ page }) => {
  await login(page);
  await page.goto('/#/account');
  const card = page.locator('[data-card="monitoring"]');
  await expect(card).toBeVisible();
  await card.locator('[data-action="toggle-prometheus"]').click();
  await expect(card.locator('[data-scrape-url]')).toContainText('/metrics');
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    card.locator('[data-action="download-grafana"]').click(),
  ]);
  expect(download.suggestedFilename()).toBe('aegis-grafana-dashboard.json');
  await card.locator('[data-action="toggle-prometheus"]').click();
  await expect(card.locator('[data-scrape-url]')).toHaveCount(0);
});
