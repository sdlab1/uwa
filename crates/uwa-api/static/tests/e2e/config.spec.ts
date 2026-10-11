import { test, expect } from './fixtures';

test.describe('config', () => {
  test('lists providers with selectors', async ({ page, gotoApp }) => {
    await gotoApp('/config');
    const chatgpt = page.locator('.card').filter({
      has: page.getByRole('heading', { name: 'chatgpt', exact: true }),
    });
    await expect(chatgpt).toContainText('#prompt-textarea');
    await expect(chatgpt).toContainText('network_first');

    const claude = page.locator('.card').filter({
      has: page.getByRole('heading', { name: 'claude', exact: true }),
    });
    await expect(claude).toContainText('div[contenteditable=true]');
  });

  test('test selector shows matched count', async ({ page, gotoApp, request }) => {
    await request.post('/__test__/selectorTest', {
      data: { matched: 5, first_text: 'hi there', duration_ms: 7, error: null },
    });
    await gotoApp('/config');

    const card = page.locator('.card').filter({
      has: page.getByRole('heading', { name: 'chatgpt', exact: true }),
    });
    await card.getByRole('button', { name: /test selector/i }).click();
    await card.getByRole('button', { name: /^Run$/ }).click();
    await expect(card.getByText(/Matched 5 element/)).toBeVisible();
    await expect(card.getByText('hi there')).toBeVisible();
  });

  test('test selector shows the error path', async ({ page, gotoApp, request }) => {
    await request.post('/__test__/selectorTest', {
      data: { matched: 0, first_text: null, duration_ms: 3, error: 'SyntaxError' },
    });
    await gotoApp('/config');

    const card = page.locator('.card').filter({
      has: page.getByRole('heading', { name: 'chatgpt', exact: true }),
    });
    await card.getByRole('button', { name: /test selector/i }).click();
    await card.getByRole('button', { name: /^Run$/ }).click();
    await expect(card.getByText(/Error: SyntaxError/)).toBeVisible();
  });
});
