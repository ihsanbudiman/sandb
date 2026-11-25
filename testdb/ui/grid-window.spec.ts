import { expect, test, type Page } from '@playwright/test';

// The results grid renders only the rows near the viewport. These cases cover what that must not break.

const mod = process.platform === 'darwin' ? 'Meta' : 'Control';

async function openCustomers(page: Page) {
  await page.goto('/');
  await expect(page.locator('.cm-content')).toBeVisible();
  const gotIt = page.getByRole('button', { name: 'Got it' });
  if (await gotIt.isVisible().catch(() => false)) await gotIt.click();
  await page.locator('.cm-content').fill('SELECT * FROM customers;');
  await page.getByRole('button', { name: 'Run', exact: true }).click();
  await expect(page.locator('#query-results tr[data-row="0"]')).toBeVisible();
}

const grid = (page: Page) => page.locator('#query-results table');
const scrollTo = (page: Page, top: number) =>
  grid(page).evaluate((t, top) => {
    t.closest('.overflow-auto')!.scrollTop = top;
  }, top);

test('a long result keeps only the visible rows in the DOM and reaches its last row', async ({ page }) => {
  await openCustomers(page);
  const total = Number(await grid(page).getAttribute('aria-rowcount')) - 1;
  expect(total).toBeGreaterThan(150);
  expect(await page.locator('#query-results tr[data-row]').count()).toBeLessThan(total);
  await scrollTo(page, 1e6);
  const last = page.locator(`#query-results tr[data-row="${total - 1}"]`);
  await expect(last).toBeInViewport();
  await expect(last.locator('td').first()).toHaveText(String(total));
});

test('arrow keys move across rows outside the rendered window', async ({ page }) => {
  await openCustomers(page);
  await page.locator('#query-results tr[data-row="0"] td').nth(1).click();
  for (let i = 0; i < 80; i++) await page.keyboard.press('ArrowDown');
  const focused = page.locator('#query-results td:focus');
  await expect(focused).toHaveCount(1);
  await expect(page.locator('#query-results tr[data-row="80"] td').nth(1)).toBeFocused();
  for (let i = 0; i < 80; i++) await page.keyboard.press('ArrowUp');
  await expect(page.locator('#query-results tr[data-row="0"] td').nth(1)).toBeFocused();
});

test('copying selected rows still works after they scroll out of the window', async ({ page, context, browserName }) => {
  await context.grantPermissions(browserName === 'webkit' ? ['clipboard-read'] : ['clipboard-read', 'clipboard-write']);
  await openCustomers(page);
  const id = await page.locator('#query-results tr[data-row="0"] td').nth(1).innerText();
  await page.getByRole('button', { name: 'Select row 1', exact: true }).click();
  await scrollTo(page, 26 * 120);
  await expect(page.locator('#query-results tr[data-row="0"]')).toHaveCount(0);
  await page.keyboard.press(`${mod}+c`);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toMatch(new RegExp(`^${id}\\t`));
});

// Chromium fires blur on a removed focused element, so only WebKit (Tauri on macOS) shows a regression here.
test('an open cell editor commits its value when its row scrolls out of the window', async ({ page }) => {
  await openCustomers(page);
  const cell = page.locator('#query-results tr[data-row="0"] td').nth(2);
  await cell.dblclick();
  const input = page.getByRole('textbox', { name: 'Cell value' });
  await expect(input).toBeFocused();
  await input.fill('Scrolled Away');
  await scrollTo(page, 26 * 120);
  await expect(page.locator('#query-results tr[data-row="0"]')).toHaveCount(0);
  await scrollTo(page, 0);
  await expect(input).toHaveCount(0);
  await expect(cell).toHaveText('Scrolled Away');
  await expect(cell).toHaveClass(/bg-warn-soft/);
});

test('the windowed table fills a wider pane and keeps its column widths while scrolling', async ({ page }) => {
  // Wide enough that the columns' content widths are narrower than the pane, so the table must stretch to fill it.
  await page.setViewportSize({ width: 1800, height: 900 });
  await openCustomers(page);
  const fill = () => grid(page).evaluate((t) => Math.abs(t.getBoundingClientRect().width - t.closest('.overflow-auto')!.clientWidth) <= 1);
  const widths = () => grid(page).evaluate((t) => [...t.tHead!.rows[0].cells].map((c) => Math.round(c.getBoundingClientRect().width)).join(','));
  expect(await fill()).toBe(true);
  const before = await widths();
  await scrollTo(page, 26 * 150);
  await page.waitForTimeout(100);
  expect(await widths()).toBe(before);
  await page.setViewportSize({ width: 2200, height: 900 });
  await expect.poll(fill).toBe(true);
});
