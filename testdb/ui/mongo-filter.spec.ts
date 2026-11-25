import { expect, test, type Page } from '@playwright/test';

async function openMongoCollection(page: Page, collection: string, rowText?: string) {
  await page.goto('/');
  const conn = page.getByRole('button', { name: /MG local mongo/ });
  if ((await conn.getAttribute('aria-expanded')) !== 'true') await conn.click();
  const db = page.getByRole('button', { name: /shop_staging/ });
  if ((await db.getAttribute('aria-expanded')) !== 'true') await db.click();
  await page.locator(`button[title^="${collection}"]`).dblclick();
  await expect(page.locator('table[aria-rowcount]')).toBeVisible();
  if (rowText) await expect(page.locator('table[aria-rowcount]')).toContainText(rowText);
}

const openMongoCustomers = (page: Page) => openMongoCollection(page, 'customers', 'Maria Lubin');

const input = (page: Page) => page.locator('.mongo-filter-input .cm-content');
const results = (page: Page) => page.locator('table[aria-rowcount]');

test('the MongoDB filter uses a Compass-style document with field completion', async ({ page }) => {
  await openMongoCustomers(page);
  await expect(page.getByText("Type a query: { field: 'value' }", { exact: true })).toBeVisible();

  await input(page).click();
  await expect(input(page)).toHaveText('{}');
  const completion = page.locator('.cm-tooltip-autocomplete');
  await expect(completion).toContainText('address.city');

  await page.keyboard.type('add');
  await expect(completion).toContainText('address.city');
  await expect(completion.locator('li').first()).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Tab');
  await expect(input(page)).toHaveText('{"address.city"}');
});

test('Enter applies a MongoDB filter document and Reset restores the collection', async ({ page }) => {
  await openMongoCustomers(page);
  await input(page).fill("{ country: 'DE' }");
  await page.keyboard.press('Enter');

  await expect(results(page)).toContainText('Maria Lubin');
  await expect(results(page)).not.toContainText('Aspen Lubin');

  const reset = page.getByRole('button', { name: 'Reset', exact: true });
  await reset.click();
  await expect(reset).toBeDisabled();
  await expect(results(page)).toContainText('Aspen Lubin');
});

test('an invalid MongoDB filter disables Find', async ({ page }) => {
  await openMongoCustomers(page);
  const find = page.getByRole('button', { name: 'Find', exact: true });
  await input(page).fill('{ country');
  await expect(find).toBeDisabled();
  await input(page).fill("{ country: 'DE' }");
  await expect(find).toBeEnabled();
});

test('completion inside $elemMatch uses paths relative to the array element', async ({ page }) => {
  await openMongoCollection(page, 'orders');
  await input(page).fill('{ items: { $elemMatch: { ');
  await page.keyboard.type('s');

  const completion = page.locator('.cm-tooltip-autocomplete');
  await expect(completion).toContainText('sku');
  await expect(completion).not.toContainText('items.sku');
});
