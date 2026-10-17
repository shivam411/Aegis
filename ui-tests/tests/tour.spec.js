// The first-run tour and the per-section "?" help. Each test gets a fresh
// browser context, so the tour counts as never seen unless login() marks it.
const { test, expect, env, login } = require('./fixtures');

test.describe.configure({ mode: 'serial' });

const overlay = (page) => page.locator('[data-tour-overlay]');
const card = (page) => page.locator('.tour-card');

/** Clicks Next until the tour ends; returns the step ids it showed. */
async function walkTour(page) {
  const seen = [];
  for (let i = 0; i < 30; i += 1) {
    if ((await overlay(page).count()) === 0) break;
    const step = await overlay(page).getAttribute('data-step');
    if (step && seen[seen.length - 1] !== step) {
      seen.push(step);
      await expect(card(page)).toContainText(`Step ${seen.length} of`);
      // A step with a target highlights it; none is drawn off-screen.
      const spot = page.locator('.tour-spot');
      if (await spot.isVisible()) {
        const box = await spot.boundingBox();
        expect(box.width).toBeGreaterThan(0);
      }
    }
    const next = card(page).locator('[data-tour-action="next"]');
    const label = await next.textContent();
    await next.click();
    if (label === 'Finish') {
      await expect(overlay(page)).toHaveCount(0);
      break;
    }
    await expect.poll(async () => ((await overlay(page).count()) === 0 ? 'gone' : overlay(page).getAttribute('data-step'))).not.toBe(step);
  }
  return seen;
}

async function tourStored(page, user = 'admin') {
  return page.evaluate((name) => localStorage.getItem(`aegis.tour.${name}`), user);
}

test('the tour appears on first sign-in, runs to the end and does not come back', async ({ page }) => {
  await login(page, env().password, { tour: true });
  await expect(overlay(page)).toBeVisible();
  await expect(card(page)).toContainText('Welcome to Aegis');
  await expect(card(page).getByRole('button', { name: 'Back' })).toBeDisabled();
  // Focus is in the step card.
  await expect(card(page).locator('[data-tour-action="next"]')).toBeFocused();

  const apps = await page.evaluate(async () => (await fetch('/api/v1/projects')).json());
  const seen = await walkTour(page);
  expect(seen[0]).toBe('welcome');
  expect(seen).toContain('new-app');
  expect(seen).toContain('conn');
  expect(seen[seen.length - 1]).toBe('shortcuts');
  if (apps.length) {
    expect(seen).toEqual(expect.arrayContaining(['deploy', 'tabs']));
  } else {
    expect(seen).not.toContain('tabs');
  }
  // Back where it started, and remembered.
  await expect(page).toHaveURL(/#\/$|\/$/);
  expect(await tourStored(page)).toBe('done');

  await page.reload();
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
  await page.waitForTimeout(1500);
  await expect(overlay(page)).toHaveCount(0);
});

test('the tour works from the keyboard, can be skipped and restarted', async ({ page }) => {
  await login(page, env().password, { tour: true });
  await expect(card(page)).toContainText('Welcome to Aegis');
  await page.keyboard.press('ArrowRight');
  await expect(overlay(page)).toHaveAttribute('data-step', 'nav-overview');
  await expect(card(page)).toContainText('Step 2 of');
  await page.keyboard.press('ArrowLeft');
  await expect(overlay(page)).toHaveAttribute('data-step', 'welcome');
  await page.keyboard.press('Enter');
  await expect(overlay(page)).toHaveAttribute('data-step', 'nav-overview');
  // Tab stays inside the card.
  for (let i = 0; i < 5; i += 1) {
    await page.keyboard.press('Tab');
    expect(await page.evaluate(() => Boolean(document.activeElement.closest('.tour-card')))).toBe(true);
  }
  // Page shortcuts are off while the tour is open.
  await page.keyboard.press('t');
  await expect(overlay(page)).toBeVisible();
  // Escape skips, and skipping counts as seen.
  await page.keyboard.press('Escape');
  await expect(overlay(page)).toHaveCount(0);
  expect(await tourStored(page)).toBe('done');

  // Account → Appearance & shortcuts replays it.
  await page.getByRole('link', { name: 'Account' }).click();
  await page.getByRole('button', { name: 'Take the tour again' }).click();
  await expect(card(page)).toContainText('Welcome to Aegis');
  await card(page).getByRole('button', { name: 'Skip tour' }).click();
  await expect(overlay(page)).toHaveCount(0);
  await expect(page).toHaveURL(/#\/account$/);

  // So does the help overlay (the ? key or the ? button at the top).
  await page.keyboard.press('?');
  const dialog = page.locator('dialog[open]');
  await expect(dialog).toContainText('Keyboard shortcuts');
  await dialog.getByRole('button', { name: 'Take the tour' }).click();
  await expect(card(page)).toContainText('Welcome to Aegis');
  await page.keyboard.press('Escape');
  await expect(overlay(page)).toHaveCount(0);
});

test('a help button explains its section', async ({ page }) => {
  await login(page);
  const btn = page.locator('[data-help="overview.host"]');
  await expect(btn).toHaveAttribute('aria-expanded', 'false');
  await btn.click();
  const pop = page.locator('[data-help-pop="overview.host"]');
  await expect(pop).toBeVisible();
  await expect(btn).toHaveAttribute('aria-expanded', 'true');
  await expect(pop.getByRole('heading', { name: 'Host resources' })).toBeVisible();
  await expect(pop).toContainText('Who can use it');
  await expect(pop.locator('[data-help-docs]')).toHaveAttribute('href', /docs\/dashboard\/overview\.md#host-tiles$/);
  // Escape closes it and gives focus back to the button.
  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
  await expect(btn).toBeFocused();
  await expect(btn).toHaveAttribute('aria-expanded', 'false');

  // A click elsewhere closes it too; the button toggles it.
  await btn.click();
  await expect(pop).toBeVisible();
  await page.getByRole('heading', { name: 'Overview' }).click();
  await expect(page.locator('.help-pop')).toHaveCount(0);
  await page.locator('[data-help="overview.apps"]').click();
  await expect(page.locator('[data-help-pop="overview.apps"]')).toContainText('Deploy');
  await page.locator('[data-help="overview.apps"]').click();
  await expect(page.locator('.help-pop')).toHaveCount(0);

  // Every main page has one.
  for (const [link, key] of [['Deployments', 'deployments'], ['Events', 'events'], ['Account', 'account']]) {
    await page.getByRole('link', { name: link, exact: true }).click();
    await page.locator(`[data-help="${key}"]`).click();
    await expect(page.locator(`[data-help-pop="${key}"]`)).toBeVisible();
    await page.keyboard.press('Escape');
  }
  for (const key of ['account.users', 'account.tokens', 'account.backups', 'account.password', 'account.appearance']) {
    await expect(page.locator(`[data-help="${key}"]`)).toBeVisible();
  }
});

test('viewers get a tour without admin or deployer steps', async ({ page }) => {
  await login(page);
  const password = `viewer-pass-${Date.now()}`;
  const status = await page.evaluate(async (pw) => {
    const { csrf_token: csrf } = await (await fetch('/api/v1/auth/session')).json();
    const res = await fetch('/api/v1/users', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
      body: JSON.stringify({ username: 'tour-viewer', password: pw, role: 'viewer' }),
    });
    return res.status;
  }, password);
  expect([200, 201]).toContain(status);
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();

  await page.getByLabel('Username').fill('tour-viewer');
  await page.getByLabel('Password').fill(password);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(card(page)).toContainText('Welcome to Aegis');
  const seen = await walkTour(page);
  expect(seen).not.toContain('new-app');
  expect(seen).not.toContain('deploy');
  expect(seen).toContain('nav-events');
  expect(await tourStored(page, 'tour-viewer')).toBe('done');
});
