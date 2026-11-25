import { expect, type Page } from '@playwright/test';

/** Refuse to type or run test SQL unless the page is using the isolated in-browser demo backend. */
export async function confirmMockBackend(page: Page) {
  await expect(page.locator('html')).toHaveAttribute('data-sandb-backend', 'mock');
}
