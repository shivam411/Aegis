// The "new Aegis version" banner, against the fake GitHub API from
// global-setup.js (v0.99.0, with nothing downloadable). The upgrade itself
// is only opened and cancelled here; installing is covered by the Rust tests.
const { test, expect, login } = require('./fixtures');

test('admins see the new version and what upgrading does', async ({ page }) => {
  await login(page);
  const banner = page.locator('[data-update="available"]');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText('Aegis v0.99.0 is available');
  await expect(banner).toContainText('(you run v');
  await expect(banner.getByRole('link', { name: 'Release notes' })).toHaveAttribute(
    'href',
    'https://github.com/shivam411/Aegis/releases/tag/v0.99.0',
  );

  // The API agrees, and says how an update would be installed here.
  const status = await page.evaluate(async () => (await fetch('/api/v1/system/update')).json());
  expect(status.update_available).toBe(true);
  expect(status.latest.version).toBe('v0.99.0');
  expect(['in_place', 'helper', 'manual', 'unsupported']).toContain(status.mode);

  if (status.can_apply) {
    await banner.getByRole('button', { name: 'Upgrade now' }).click();
    const dialog = page.locator('dialog[open]');
    await expect(dialog).toContainText('Upgrade Aegis to v0.99.0?');
    await expect(dialog).toContainText('backup');
    await expect(dialog).toContainText('SHA-256');
    await expect(dialog).toContainText('apps are stopped');
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(dialog).toHaveCount(0);
  } else if (status.mode === 'manual') {
    await expect(banner.locator('code')).toContainText('self-update');
  }
  // Nothing was started.
  const after = await page.evaluate(async () => (await fetch('/api/v1/system/update')).json());
  expect(after.phase).toBe('idle');

  // "Later" hides it for this version, also after a reload.
  await banner.getByRole('button', { name: 'Later' }).click();
  await expect(banner).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
  await expect(page.locator('[data-update="available"]')).toHaveCount(0);
});
