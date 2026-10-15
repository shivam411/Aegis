// Browser tests for the dashboard. global-setup.js starts a real daemon
// (built binaries from ../target) against a scratch data directory.
const { defineConfig, devices } = require('@playwright/test');

const WEB_PORT = Number(process.env.AEGIS_E2E_WEB_PORT || 18640);

module.exports = defineConfig({
  testDir: './tests',
  // One daemon, shared state: run the scenario in order.
  workers: 1,
  fullyParallel: false,
  timeout: 180_000,
  expect: { timeout: 30_000 },
  retries: 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  globalSetup: require.resolve('./global-setup.js'),
  globalTeardown: require.resolve('./global-teardown.js'),
  use: {
    baseURL: `http://127.0.0.1:${WEB_PORT}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'] }, testIgnore: /mobile/ },
    { name: 'mobile', use: { ...devices['Pixel 7'] }, testMatch: /mobile/, dependencies: ['desktop'] },
  ],
});
