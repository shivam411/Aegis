// Screenshots for the dashboard guide (docs/dashboard/): `npm run screenshots`.
// Starts its own daemon (screenshots/setup.js), seeds demo apps, users,
// tokens, backups and events, and writes PNGs to docs/images/dashboard/.
const { defineConfig, devices } = require('@playwright/test');

const WEB_PORT = Number(process.env.AEGIS_SHOTS_WEB_PORT || 18740);

module.exports = defineConfig({
  testDir: './screenshots',
  testMatch: /\.shots\.js$/,
  workers: 1,
  fullyParallel: false,
  timeout: 1_800_000,
  expect: { timeout: 60_000 },
  retries: 0,
  reporter: 'list',
  globalSetup: require.resolve('./screenshots/setup.js'),
  globalTeardown: require.resolve('./screenshots/teardown.js'),
  use: {
    ...devices['Desktop Chrome'],
    baseURL: `http://127.0.0.1:${WEB_PORT}`,
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1,
    colorScheme: 'light',
    reducedMotion: 'reduce',
    timezoneId: 'UTC',
    locale: 'en-US',
    trace: 'retain-on-failure',
  },
  outputDir: './test-results-shots',
});
