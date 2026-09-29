// Phase 4: watch an app's CPU and memory, and change its limits live from
// the browser. Where the daemon can't use cgroups (e.g. CI without root),
// the page must say so and keep the sliders disabled.
const { test, expect, env, login, confirm } = require('./fixtures');

const HOG_PORT = Number(process.env.AEGIS_E2E_HOG_PORT || 18651);
const MIB = 1024 * 1024;

async function get(path) {
  try {
    const res = await fetch(`http://127.0.0.1:${HOG_PORT}${path}`, { signal: AbortSignal.timeout(5000) });
    return res.status;
  } catch {
    return null;
  }
}

test.describe.configure({ mode: 'serial' });

test('resource charts, live limits, CPU cap and OOM restart', async ({ page }) => {
  const { hog } = env();
  await login(page);

  await test.step('create the app from a server directory', async () => {
    await page.goto('/#/apps/new');
    await page.getByRole('radio', { name: 'Directory on this server' }).check();
    await page.getByRole('textbox', { name: 'Directory on this server' }).fill(hog);
    await page.getByLabel('App name', { exact: true }).fill('hog');
    await page.getByRole('button', { name: 'Detect settings →' }).click();
    await expect(page.getByText('Detected: Python')).toBeVisible();
    await page.getByLabel('Install command').fill('');
    await page.getByLabel('Build command').fill('');
    await page.getByLabel('Start command').fill('exec python3 app.py');
    await page.getByLabel('Port').fill(String(HOG_PORT));
    await page.getByLabel('Health check URL').fill(`http://127.0.0.1:${HOG_PORT}/`);
    await page.getByRole('button', { name: 'Create app' }).click();
    await expect(page.locator('[data-deployment] [data-status="Success"]')).toBeVisible({ timeout: 90_000 });
  });

  const caps = await (await page.request.get('/api/v1/resources')).json();
  const limitsAvailable = caps.cpu && caps.memory;

  await test.step('usage is measured and charted', async () => {
    await page.getByRole('link', { name: 'Resources' }).click();
    await expect(page.locator('[data-stat="memory"] .value')).toHaveText(/\d+(\.\d+)? (KB|MB)/);
    await expect(page.locator('[data-stat="threads"] .value')).toContainText('process');
    await expect(page.getByRole('img', { name: 'CPU chart' })).toBeVisible();
    await expect(page.getByRole('img', { name: 'Memory chart' })).toBeVisible();
    // The live chart fills in from the stream.
    await expect(page.locator('.chart-readout').nth(1)).toContainText(/MB/, { timeout: 15_000 });
    await page.getByRole('button', { name: '1 hour' }).click();
    await expect(page.getByRole('button', { name: '1 hour' })).toHaveAttribute('aria-pressed', 'true');
    await page.getByRole('button', { name: 'Live · 5 min' }).click();
  });

  if (!limitsAvailable) {
    await test.step('limits are explained as unavailable', async () => {
      await expect(page.locator('[data-unsupported]')).toContainText('not available');
      await expect(page.getByLabel('CPU limit in cores')).toBeDisabled();
      await expect(page.getByLabel('Memory limit in MiB')).toBeDisabled();
    });
    return;
  }

  await test.step('memory limit applies within a second, without a restart', async () => {
    const before = await (await page.request.get('/api/v1/projects/hog')).json();
    await page.getByLabel('No limit').nth(1).uncheck();
    await expect(page.locator('[data-applied]')).toContainText('Applied');
    const started = Date.now();
    await page.getByLabel('Memory limit (MiB)').fill('96');
    await page.getByLabel('Memory limit (MiB)').press('Enter');
    await expect.poll(async () => (await (await page.request.get('/api/v1/projects/hog')).json()).resources.memory_bytes, { timeout: 5000 })
      .toBe(96 * MIB);
    expect(Date.now() - started).toBeLessThan(1500);
    await expect(page.locator('.toast', { hasText: 'memory 96.0 MB' })).toBeVisible();
    const after = await (await page.request.get('/api/v1/projects/hog')).json();
    expect(after.process.pid).toBe(before.process.pid);
    await expect(page.locator('[data-stat="memory"] .value')).toContainText('/ 96.0 MB');
  });

  await test.step('a limit below current use asks first', async () => {
    await page.getByLabel('Memory limit (MiB)').fill('8');
    await page.getByLabel('Memory limit (MiB)').press('Enter');
    await expect(page.locator('dialog[open]')).toContainText('is using');
    await confirm(page, 'Cancel');
    await expect(page.getByLabel('Memory limit (MiB)')).toHaveValue('96');
    const p = await (await page.request.get('/api/v1/projects/hog')).json();
    expect(p.resources.memory_bytes).toBe(96 * MIB);
  });

  await test.step('dragging the CPU slider caps a busy loop', async () => {
    await page.getByLabel('No limit').first().uncheck();
    const slider = page.getByLabel('CPU limit in cores');
    // Drag to the far left (0.1 cores), then one step right: 0.15... use the
    // keyboard for an exact value after the drag.
    const box = await slider.boundingBox();
    await page.mouse.move(box.x + box.width * 0.6, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + 1, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    await expect(page.locator('[data-value="cpu"]')).toHaveText('0.10 cores');
    await slider.press('ArrowRight');
    await slider.press('ArrowRight');
    await expect(page.locator('[data-value="cpu"]')).toHaveText('0.20 cores');
    await expect.poll(async () => (await (await page.request.get('/api/v1/projects/hog')).json()).resources.cpu_cores)
      .toBe(0.2);

    expect(await get('/spin')).toBe(200);
    // Throttled to the limit: once the loop has run for a whole sample
    // interval, samples settle at about 0.2 cores and never go above it.
    const steady = [];
    await expect.poll(async () => {
      const r = await (await page.request.get('/api/v1/projects/hog/resources')).json();
      if (r.usage && r.usage.throttled_ratio > 0.3 && !steady.some((u) => u.at === r.usage.at)) steady.push(r.usage);
      return steady.length;
    }, { timeout: 30_000, intervals: [500] }).toBeGreaterThanOrEqual(4);
    const settled = steady.slice(1);
    for (const u of settled) expect(u.cpu_cores).toBeLessThan(0.3);
    expect(Math.max(...settled.map((u) => u.cpu_cores))).toBeGreaterThan(0.12);
    await expect(page.locator('.chart-readout').first()).toContainText('limit 0.20 cores');
    await expect(page.locator('[data-stat="throttled"] .value')).not.toHaveText('0%');
  });

  await test.step('going over the memory limit is killed, restarted and reported', async () => {
    await get('/hog');
    await page.getByRole('link', { name: 'Activity' }).click();
    const alert = page.locator('[data-event-type="ResourcePressureDetected"]').first();
    await expect(alert).toContainText('went over its 96 MiB memory limit', { timeout: 20_000 });
    await expect(page.locator('[data-event-type="ProcessCrashed"]').first()).toBeVisible();
    await expect(page.locator('.page-header')).toContainText(/[1-9]\d* restarts?/);
    await expect(page.locator('.page-header [data-status="Running"]')).toBeVisible();
    await expect.poll(() => get('/')).toBe(200);
    // Limits survive the restart.
    const p = await (await page.request.get('/api/v1/projects/hog')).json();
    expect(p.resources).toEqual({ cpu_cores: 0.2, memory_bytes: 96 * MIB, pids_max: null });

    await page.getByRole('link', { name: 'Events' }).click();
    await page.getByLabel('Category').selectOption('resources');
    await expect(page.locator('[data-events] [data-event-type="ResourceLimitsChanged"]').first()).toContainText('by user:admin');
  });

  await test.step('overview shows usage against the limits', async () => {
    await page.getByRole('link', { name: 'Overview' }).click();
    const card = page.locator('[data-app="hog"]');
    await expect(card.locator('[data-fact="memory"] dd')).toContainText('/ 96.0 MB');
    await expect(card.locator('[data-fact="cpu"] dd')).toContainText('/ 0.2 cores');
  });
});
