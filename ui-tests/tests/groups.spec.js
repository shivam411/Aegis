// App groups and "new commits": put "shop" (deployed by dashboard.spec.js,
// then rolled back to v1 while main has v2) in a group, see the group card
// and its "1 new commit" badge, deploy the changed apps, and remove the
// group again so the later specs see "shop" on its own.
const { test, expect, env, login, confirm, appBody } = require('./fixtures');

const APP_PORT = Number(process.env.AEGIS_E2E_APP_PORT || 18650);

test.describe.configure({ mode: 'serial' });

test('a group card with new commits, deployed from the overview', async ({ page }) => {
  const { repo } = env();
  await login(page);

  await test.step('create a group with shop in it', async () => {
    await page.locator('[data-action="new-group"]').click();
    await page.getByLabel('Group name').fill('shop-group');
    await page.getByLabel('Repository URL').fill(repo);
    await page.getByLabel('Branch').fill('main');
    await page.getByLabel('Add shop').check();
    await page.getByRole('button', { name: 'Create group' }).click();
    await expect(page).toHaveURL(/#\/groups\/shop-group$/);
    await expect(page.locator('tr[data-member="shop"]')).toBeVisible();
  });

  await test.step('check for new commits on the group page', async () => {
    const head = page.locator('.page-header [data-group-updates]');
    await head.getByRole('button', { name: 'Check now' }).click();
    await expect(head).toContainText('1 new commit');
    await expect(page.locator('tr[data-member="shop"] [data-changed]')).toBeVisible();
    await expect(page.locator('[data-group-commits] [data-commits]')).toContainText('v2');
    // Watch paths are saved per app.
    const row = page.locator('tr[data-member="shop"]');
    await row.getByLabel('Watch paths of shop').fill('index.js, package.json');
    await row.getByRole('button', { name: 'Save' }).click();
    await expect(page.locator('.toast', { hasText: 'Watch paths of shop saved' })).toBeVisible();
  });

  await test.step('the overview shows the group as one card', async () => {
    await page.getByRole('link', { name: 'Overview' }).click();
    const card = page.locator('[data-group="shop-group"]');
    await expect(card).toBeVisible();
    await expect(card.locator('tr[data-member="shop"]')).toBeVisible();
    await expect(card.locator('[data-group-updates]')).toContainText('1 new commit');
    await expect(card.locator('tr[data-member="shop"] [data-changed]')).toBeVisible();
    await expect(page.locator('[data-app="shop"]')).toHaveCount(0);

    await card.getByRole('button', { name: 'Deploy changed only' }).click();
    await expect(card.locator('[data-group-run="Success"]')).toBeVisible({ timeout: 120_000 });
    await expect.poll(async () => (await appBody(APP_PORT))?.version, { timeout: 60_000 }).toBe('v2');
  });

  await test.step('up to date afterwards; the app links to its group', async () => {
    await page.locator('[data-group="shop-group"]').getByRole('link', { name: 'shop-group' }).click();
    await page.locator('.page-header [data-group-updates]').getByRole('button', { name: 'Check now' }).click();
    await expect(page.locator('tr[data-member="shop"] [data-member-updates]')).toContainText('Up to date');
    await page.locator('tr[data-member="shop"]').getByRole('link', { name: 'shop' }).click();
    await expect(page.locator('[data-group-link]')).toHaveText('Group shop-group');
    await expect(page.locator('[data-card="updates"] [data-up-to-date]')).toBeVisible();
  });

  await test.step('delete the group: shop is on its own again', async () => {
    await page.goto('/#/groups/shop-group');
    await page.getByRole('button', { name: 'Delete group' }).click();
    await confirm(page, 'Delete group');
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
    await expect(page.locator('[data-app="shop"]')).toBeVisible();
    await expect(page.locator('[data-group="shop-group"]')).toHaveCount(0);
  });
});
