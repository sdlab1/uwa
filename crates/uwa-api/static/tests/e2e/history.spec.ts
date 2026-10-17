import { test, expect } from './fixtures';

test.describe('history', () => {
  test('renders records from /admin/history', async ({ page, gotoApp }) => {
    await gotoApp('/history');
    const table = page.locator('table').first();
    await expect(table.getByRole('cell', { name: 'chatgpt', exact: true })).toBeVisible();
    await expect(table.getByRole('cell', { name: 'gpt-4o', exact: true })).toBeVisible();
    await expect(table.getByRole('cell', { name: 'claude-3-5-sonnet' })).toBeVisible();
  });

  test('detail modal shows raw JSON and closes', async ({ page, gotoApp }) => {
    await gotoApp('/history');
    await page.locator('table').first().getByRole('cell', { name: 'chatgpt', exact: true }).click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('Record req_aaa');
    await expect(dialog).toContainText('"user_preview"');

    await dialog.getByRole('button', { name: 'close' }).click();
    await expect(dialog).not.toBeVisible();
  });

  test('empty state', async ({ page, gotoApp, request }) => {
    await request.post('/__test__/history', {
      data: { count: 0, buffer_size: 0, total_matching: 0, next_before_id: null, records: [] },
    });
    await gotoApp('/history');
    await expect(page.getByText('No records match the filters', { exact: false })).toBeVisible();
  });
});
