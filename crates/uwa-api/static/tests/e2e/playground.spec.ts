import { test, expect } from './fixtures';

test.describe('playground', () => {
  test('non-streaming round trip', async ({ page, gotoApp }) => {
    await gotoApp('/playground');
    await page.locator('.composer-toggle input[type=checkbox]').uncheck();
    await page.locator('.composer-textarea').fill('hi');
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect(page.getByText('Hello from the site.', { exact: false })).toBeVisible();
  });

  test('streaming shows TTFT and reaches done', async ({ page, gotoApp }) => {
    await gotoApp('/playground');
    await page.locator('.composer-textarea').fill('stream please');
    await page.getByRole('button', { name: 'Send', exact: true }).click();

    const badge = page.locator('.stream-badge');
    await expect(badge).toBeVisible();

    // Final aggregated text lands in the assistant bubble.
    await expect(page.getByText('Hello from the site.', { exact: false })).toBeVisible({ timeout: 8000 });
    await expect(page.getByText('This is a complete reply.', { exact: false })).toBeVisible({ timeout: 8000 });

    // Badge reports done + TTFT.
    await expect(badge).toContainText(/done/, { timeout: 8000 });
    await expect(badge).toContainText(/TTFT/);
  });

  test('cancel stops an in-flight stream', async ({ page, gotoApp }) => {
    await gotoApp('/playground');
    await page.locator('.composer-textarea').fill('cancel me');
    await page.getByRole('button', { name: 'Send', exact: true }).click();

    const cancel = page.getByRole('button', { name: 'Cancel', exact: true });
    await expect(cancel).toBeVisible();
    await cancel.click();

    // Send returns once the abort settles.
    await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeVisible({ timeout: 5000 });
  });

  test('conversation survives a reload', async ({ page, gotoApp }) => {
    await gotoApp('/playground');
    await page.locator('.composer-toggle input[type=checkbox]').uncheck();
    await page.locator('.composer-textarea').fill('persist me');
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect(page.getByText('Hello from the site.', { exact: false })).toBeVisible();

    await page.reload();
    await expect(page.locator('header')).toBeVisible();
    // The turn survived: user bubble + assistant bubble are back.
    await expect(page.locator('.bubble-user', { hasText: 'persist me' })).toBeVisible();
    await expect(page.locator('.bubble-assistant', { hasText: 'Hello from the site.' })).toBeVisible();
  });
});
