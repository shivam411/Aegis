// The dashboard on a phone-sized screen: everything reachable, nothing
// wider than the viewport.
const { test, expect, login } = require('./fixtures');

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
});
