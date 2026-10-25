// Hidden apps and per-user app lists. Runs after the other specs and uses
// whichever app they left behind; everything is restored at the end.
const { test, expect, login, confirm } = require('./fixtures');

const HIDE_PW = 'e2e-hide-password-1';

test.describe.configure({ mode: 'serial' });

test('hide an app behind a password and unlock it again', async ({ page }) => {
  await login(page);
  const card = page.locator('[data-app]').first();
  await expect(card).toBeVisible();
  const name = await card.getAttribute('data-app');

  await test.step('hide it', async () => {
    await page.goto(`/#/apps/${name}/settings`);
    await page.getByLabel('Hide password').fill(HIDE_PW);
    await page.locator('[data-action="hide-app"]').click();
    await expect(page.locator('.toast', { hasText: `${name} is hidden` })).toBeVisible();
    await expect(page.locator('[data-card="hide-app"]')).toContainText('hidden behind a password');
  });

  await test.step('hiding it again removes it from the overview', async () => {
    await page.locator('[data-action="lock-app"]').click();
    await expect(page).toHaveURL(/#\/$/);
    await expect(page.locator(`[data-app="${name}"]`)).toHaveCount(0);
  });

  await test.step('a wrong password does not unlock it', async () => {
    await page.locator('[data-action="unlock-app"]').click();
    const dlg = page.locator('dialog[open]');
    await dlg.getByLabel('App name').fill(name);
    await dlg.getByLabel('Hide password').fill('not-the-password');
    await dlg.getByRole('button', { name: 'Unlock' }).click();
    await expect(page.locator('.toast.bad')).toBeVisible();
    await expect(page.locator(`[data-app="${name}"]`)).toHaveCount(0);
  });

  await test.step('the right password shows it with a Hidden badge', async () => {
    await page.locator('[data-action="unlock-app"]').click();
    const dlg = page.locator('dialog[open]');
    await dlg.getByLabel('App name').fill(name);
    await dlg.getByLabel('Hide password').fill(HIDE_PW);
    await dlg.getByRole('button', { name: 'Unlock' }).click();
    await expect(page).toHaveURL(new RegExp(`#/apps/${name}`));
    await expect(page.locator('.page-header [data-hidden-app]')).toBeVisible();
  });

  await test.step('stop hiding it', async () => {
    await page.goto(`/#/apps/${name}/settings`);
    await page.locator('[data-action="unhide-app"]').click();
    await confirm(page, 'Stop hiding');
    await expect(page.locator('.toast', { hasText: 'no longer hidden' })).toBeVisible();
    await page.goto('/#/');
    await expect(page.locator(`[data-app="${name}"]`)).toBeVisible();
    await expect(page.locator(`[data-app="${name}"] [data-hidden-app]`)).toHaveCount(0);
  });
});

test('limit an account to some apps', async ({ page }) => {
  await login(page);
  const name = await page.locator('[data-app]').first().getAttribute('data-app');
  await page.goto('/#/account');
  await page.locator('#user-name').fill('limited');
  await page.locator('#user-role').selectOption('viewer');
  await page.locator('.app-access input[value="some"]').first().check();
  await page.locator(`.app-access input[aria-label="Can see ${name}"]`).first().check();
  await page.locator('[data-action="create-user"]').click();
  await expect(page.locator('tr[data-user="limited"]')).toBeVisible();
  const row = page.locator('tr[data-user="limited"]');
  await expect(row.locator('[data-action="user-apps"]')).toHaveText('1 app');

  await test.step('switch it back to all apps', async () => {
    await row.locator('[data-action="user-apps"]').click();
    const dlg = page.locator('dialog[open]');
    await dlg.getByLabel('All apps').check();
    await dlg.getByRole('button', { name: 'Save' }).click();
    await expect(row.locator('[data-action="user-apps"]')).toHaveText('All apps');
  });

  await test.step('clean up', async () => {
    await row.locator('[data-action="delete-user"]').click();
    await confirm(page, 'Delete');
    await expect(row).toHaveCount(0);
  });
});

test('switch to the list view and restart the selected apps', async ({ page }) => {
  await login(page);
  const first = page.locator('[data-app]').first();
  await expect(first).toBeVisible();
  const name = await first.getAttribute('data-app');

  await page.locator('[data-view="list"]').click();
  await expect(page.locator('table.apps-table')).toBeVisible();
  await page.reload();
  await expect(page.locator('table.apps-table')).toBeVisible();

  await page.locator(`[data-select="${name}"]`).check();
  await expect(page.locator('[data-selected-count]')).toHaveText('1 selected');
  await page.locator('[data-bulk="deploy"]').click();
  await confirm(page, 'Deploy');
  await expect(page.locator('.toast', { hasText: 'queued for deployment' })).toBeVisible();

  await page.locator('[data-bulk="clear"]').click();
  await expect(page.locator('[data-selected-count]')).toHaveCount(0);
  await page.locator('[data-view="grid"]').click();
  await expect(page.locator('table.apps-table')).toHaveCount(0);
});
