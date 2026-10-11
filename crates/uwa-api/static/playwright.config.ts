import { defineConfig, devices } from '@playwright/test';

const PORT = Number(process.env.MOCK_PORT ?? 9876);
const BASE = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './tests/e2e',
  // The mock server is shared per run; each test resets it via /__test__/reset.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['github'], ['list']] : [['list']],
  timeout: 20_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL: BASE,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'off',
    actionTimeout: 5_000,
    navigationTimeout: 10_000,
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],
  webServer: {
    command: 'node tests/e2e/mock-server.mjs',
    url: `${BASE}/healthz`,
    reuseExistingServer: !process.env.CI,
    timeout: 15_000,
    env: { MOCK_PORT: String(PORT) },
  },
});
