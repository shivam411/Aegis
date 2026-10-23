// "Scan a folder on the server" in the new app wizard (import without
// deploying, hard-coded ports, .env names only) and the deploy key panel in
// Settings → Repository (public key, fingerprint, GitHub link, never the
// private key).
const { test, expect, env, login } = require('./fixtures');

test.describe.configure({ mode: 'serial' });

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

test('scan a folder, import the apps, set up a deploy key', async ({ page }) => {
  const { scanDir } = env();
  await login(page);

  await test.step('scan the folder', async () => {
    await page.goto('/#/apps/new');
    await page.locator('[data-kind="scan"]').check();
    const folder = page.getByLabel('Folder on this server', { exact: true });
    await expect(folder).toHaveValue(scanDir);
    await page.getByRole('button', { name: 'Scan →' }).click();
    const results = page.locator('[data-scan-results]');
    await expect(results).toContainText('Found 2 apps');

    const img = results.locator('tr[data-candidate$="img-man"]');
    // The hard-coded port becomes $PORT; the port itself is kept.
    await expect(img.getByLabel('Start command for img-man')).toHaveValue('npx next start -p $PORT');
    await expect(img.getByLabel('Port for img-man')).toHaveValue('4000');
    await expect(img.locator('[data-warning="hardcoded_port"]')).toBeVisible();
    // An SSH remote needs a deploy key; the .env shows names only.
    await expect(img).toContainText('git@github.com:acme/img-man.git @ main');
    await expect(img.locator('[data-warning="ssh_remote"]')).toBeVisible();
    await expect(img).toContainText('MONGODB_URI');
    expect(await page.content()).not.toContain('s3ntinel');

    const notes = results.locator('tr[data-candidate$="notes-api"]');
    await expect(notes).toContainText('Python');
    await expect(notes.locator('[data-warning="no_remote"]')).toBeVisible();
  });

  await test.step('import both without deploying', async () => {
    await page.getByRole('button', { name: 'Import selected' }).click();
    const outcome = page.locator('[data-import-result]');
    await expect(outcome.locator('[data-imported="img-man"]')).toContainText('Imported');
    await expect(outcome.locator('[data-imported="notes-api"]')).toContainText('Imported');
    await expect(outcome.locator('[data-imported="img-man"]')).toContainText('deploy key');

    const projects = JSON.parse((await apiCall(page, 'GET', '/projects')).text);
    const img = projects.find((p) => p.name === 'img-man');
    const notes = projects.find((p) => p.name === 'notes-api');
    expect(img.repository_url).toBe('git@github.com:acme/img-man.git');
    expect(img.settings.start_command).toBe('npx next start -p $PORT');
    expect(img.settings.port).toBe(4000);
    expect(notes.source_dir).toContain('notes-api');
    for (const p of [img, notes]) {
      expect(p.current_release).toBeNull();
      const deployments = JSON.parse((await apiCall(page, 'GET', `/deployments?project=${p.id}`)).text);
      expect(deployments).toEqual([]);
    }

    // Scanning again lists them as already imported.
    await page.getByRole('button', { name: 'Scan →' }).click();
    await expect(page.locator('tr[data-candidate$="img-man"]')).toHaveAttribute('data-registered', 'yes');
    await expect(page.locator('tr[data-candidate$="notes-api"]')).toContainText('Already imported as notes-api');
  });

  await test.step('generate a deploy key', async () => {
    await page.goto('/#/apps/img-man/settings');
    const card = page.locator('[data-card="repository"]');
    await card.getByRole('button', { name: 'Generate deploy key' }).click();
    await expect(card.locator('[data-public-key]')).toContainText('ssh-ed25519 ');
    await expect(card.locator('[data-fingerprint]')).toContainText('SHA256:');
    await expect(card.locator('[data-add-key-link]'))
      .toHaveAttribute('href', 'https://github.com/acme/img-man/settings/keys/new');
    await expect(card).toContainText('Allow write access');
    expect(await page.content()).not.toContain('PRIVATE KEY');

    // The key survives a reload, and the API never returns the private half.
    await page.reload();
    await expect(page.locator('[data-card="repository"] [data-public-key]')).toContainText('ssh-ed25519 ');
    const key = await apiCall(page, 'GET', '/projects/img-man/deploy-key');
    expect(key.status).toBe(200);
    expect(key.text).not.toContain('PRIVATE');
  });
});
