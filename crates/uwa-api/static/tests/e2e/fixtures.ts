import { test as base, expect } from '@playwright/test';

type Fixtures = {
  /**
   * Reset the mock server, mark the setup wizard as complete and the API
   * key as set, then navigate to `#<hash>`. Returns once the shell has
   * mounted, so callers can start asserting immediately.
   */
  gotoApp: (hash?: string) => Promise<void>;
};

export const test = base.extend<Fixtures>({
  gotoApp: async ({ page, request }, use) => {
    await request.post('/__test__/reset');
    await page.addInitScript(() => {
      localStorage.setItem('uwa:setup:done', '1');
      localStorage.setItem('uwa:apiKey', 'test-key');
    });
    await use(async (hash = '/') => {
      await page.goto(`/#${hash}`);
      await expect(page.locator('header')).toBeVisible();
    });
  },
});

export { expect };
