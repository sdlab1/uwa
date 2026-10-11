import { test, expect } from './fixtures';

test.describe('shell', () => {
  test('boots with topbar and nav', async ({ page, gotoApp }) => {
    await gotoApp('/');
    await expect(page.locator('header')).toContainText('uwa');
    const nav = page.locator('nav');
    for (const label of ['Dashboard', 'Playground', 'History', 'Sessions', 'Config', 'Logs', 'Debug']) {
      await expect(nav.getByRole('link', { name: label })).toBeVisible();
    }
  });

  test('navigates between routes', async ({ page, gotoApp }) => {
    await gotoApp('/');
    await page.locator('nav').getByRole('link', { name: 'Playground' }).click();
    await expect(page).toHaveURL(/#\/playground/);
    // The Playground header carries the conversation title; the composer
    // is the route's unique landmark.
    await expect(page.locator('.composer-textarea')).toBeVisible();

    await page.locator('nav').getByRole('link', { name: 'History' }).click();
    await expect(page).toHaveURL(/#\/history/);
    await expect(page.getByRole('heading', { name: 'History', level: 2 })).toBeVisible();
  });

  test('server pill turns ok', async ({ page, gotoApp }) => {
    await gotoApp('/');
    const pill = page.locator('header .pill').filter({ hasText: /server/ });
    await expect(pill).toContainText(/ok|degraded/, { timeout: 8000 });
  });

  test('first-run redirects to setup', async ({ page, request }) => {
    await request.post('/__test__/reset');
    // No init script: `uwa:setup:done` is unset.
    await page.goto('/');
    await expect(page).toHaveURL(/#\/setup/);
    await expect(page.getByRole('heading', { name: 'Setup', level: 2 })).toBeVisible();
  });
});
