import { test, expect } from './fixtures';

test.describe('logs', () => {
  test('streams four events and reports connected', async ({ page, gotoApp }) => {
    await gotoApp('/logs');
    await expect(page.getByText('connected', { exact: false })).toBeVisible({ timeout: 5000 });
    await expect(page.locator('.log-line')).toHaveCount(4, { timeout: 5000 });
  });

  test('level filter narrows the list', async ({ page, gotoApp }) => {
    await gotoApp('/logs');
    await expect(page.locator('.log-line')).toHaveCount(4, { timeout: 5000 });
    await page.getByRole('button', { name: /^Pause$/ }).click();

    await page.locator('select').first().selectOption('ERROR');
    await expect(page.locator('.log-line')).toHaveCount(1);
    await expect(page.locator('.log-line')).toContainText('ERROR');

    await page.locator('select').first().selectOption('INFO');
    await expect(page.locator('.log-line')).toHaveCount(2);

    await page.locator('select').first().selectOption('');
    await expect(page.locator('.log-line')).toHaveCount(4);
  });

  test('pause freezes the list and clear empties it', async ({ page, gotoApp }) => {
    await gotoApp('/logs');
    await expect(page.locator('.log-line')).toHaveCount(4, { timeout: 5000 });
    await page.getByRole('button', { name: /^Pause$/ }).click();
    await page.getByRole('button', { name: /^Clear$/ }).click();
    // With pause on, no new events land; the list stays empty.
    await page.waitForTimeout(300);
    await expect(page.locator('.log-line')).toHaveCount(0);
  });
});
