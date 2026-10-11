import { test, expect } from './fixtures';

test.describe('dashboard', () => {
  test('renders KPIs and TTFT from /admin/stats', async ({ page, gotoApp, request }) => {
    await request.post('/__test__/stats', {
      data: {
        total: 123, success: 120, error: 3,
        avg_total_ms: 810, p50_total_ms: 700, p95_total_ms: 2000,
        avg_ttft_ms: 300, p50_ttft_ms: 280, p95_ttft_ms: 800,
      },
    });
    await gotoApp('/');

    const overview = page.locator('.card').filter({ hasText: 'Overview' }).first();
    await expect(overview.getByText('123')).toBeVisible();
    await expect(overview.getByText('120')).toBeVisible();

    // TTFT card shows p50 = 280 ms.
    const ttft = page.locator('.card').filter({ hasText: 'Time to first token' });
    await expect(ttft.getByText('280 ms')).toBeVisible();
  });

  test('shows per-provider table', async ({ page, gotoApp }) => {
    await gotoApp('/');
    const table = page.locator('.card').filter({ hasText: 'Per provider' });
    await expect(table.getByRole('cell', { name: 'chatgpt', exact: true })).toBeVisible();
    await expect(table.getByRole('cell', { name: 'claude', exact: true })).toBeVisible();
  });

  test('window switcher refetches with the new window', async ({ page, gotoApp }) => {
    const urls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/admin/stats')) urls.push(r.url());
    });
    await gotoApp('/');
    const before = urls.length;
    await page.getByRole('button', { name: 'last50', exact: true }).click();
    await expect.poll(() => urls.length).toBeGreaterThan(before);
    expect(urls[urls.length - 1]).toContain('window=last50');
  });

  test('pool panel lists tab URLs', async ({ page, gotoApp }) => {
    await gotoApp('/');
    const pool = page.locator('.card').filter({ hasText: 'Tab pool' });
    await expect(pool.getByText(/chatgpt\.com/)).toBeVisible();
    await expect(pool.getByText(/claude\.ai/)).toBeVisible();
  });

  test('error state when stats fail', async ({ page, gotoApp }) => {
    await page.route('**/admin/stats*', (route) =>
      route.fulfill({
        status: 500,
        contentType: 'application/json',
        body: JSON.stringify({ error: { message: 'boom', code: 'internal_error' } }),
      }),
    );
    await gotoApp('/');
    await expect(page.getByText('Cannot reach stats')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
  });
});
