// The dashboard on a phone-sized screen: everything reachable, nothing
// wider than the viewport.
const { test, expect, env, login } = require('./fixtures');

async function noHorizontalScroll(page) {
  const [scroll, width] = await page.evaluate(() => [document.documentElement.scrollWidth, window.innerWidth]);
  expect(scroll).toBeLessThanOrEqual(width);
}

test('works on a small screen', async ({ page }) => {
  await login(page);
  await noHorizontalScroll(page);
  await expect(page.locator('[data-app="shop"]')).toBeVisible();
  await page.locator('[data-app="shop"]').getByRole('link', { name: 'shop' }).click();
  await expect(page.locator('[data-stage="Promote"]')).toBeVisible();
  await noHorizontalScroll(page);
  for (const tab of ['Releases', 'Logs', 'Resources', 'Settings', 'Activity']) {
    await page.getByRole('link', { name: tab }).click();
    await noHorizontalScroll(page);
  }
  await page.getByRole('link', { name: 'Deployments' }).click();
  await expect(page.locator('tr[data-deployment]').first()).toBeVisible();
  await noHorizontalScroll(page);
  await page.getByRole('link', { name: 'Events' }).click();
  await noHorizontalScroll(page);

  // Section help opens as a sheet inside the screen.
  await page.locator('[data-help="events"]').click();
  const pop = page.locator('[data-help-pop="events"]');
  await expect(pop).toBeVisible();
  const box = await pop.boundingBox();
  const width = page.viewportSize().width;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(width);
  await pop.getByRole('button', { name: 'Close help' }).click();
  await expect(pop).toHaveCount(0);
});

test('the tour fits a small screen', async ({ page }) => {
  await login(page, env().password, { tour: true });
  const card = page.locator('.tour-card');
  await expect(card).toContainText('Welcome to Aegis');
  const width = page.viewportSize().width;
  const height = page.viewportSize().height;
  for (let i = 0; i < 4; i += 1) {
    const box = await card.boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.y + box.height).toBeLessThanOrEqual(height);
    await card.locator('[data-tour-action="next"]').click();
  }
  await page.keyboard.press('Escape');
  await expect(page.locator('[data-tour-overlay]')).toHaveCount(0);
  await noHorizontalScroll(page);
});
