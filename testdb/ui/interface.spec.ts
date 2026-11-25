import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';

async function runQuery(page: Page) {
  const editor = page.locator('.cm-content');
  await editor.fill('SELECT * FROM customers;');
  await page.getByRole('button', { name: 'Run', exact: true }).click();
  await expect(page.locator('#query-results table')).toBeVisible();
}

test.beforeEach(async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('.cm-content')).toBeVisible();
});

test('sidebar tabs support arrows, Home/End, and a single tab stop', async ({ page }) => {
  const connections = page.getByRole('tab', { name: 'Connections', exact: true });
  const queries = page.getByRole('tab', { name: /^Queries/ });
  await connections.focus();
  await page.keyboard.press('ArrowRight');
  await expect(queries).toBeFocused();
  await expect(queries).toHaveAttribute('aria-selected', 'true');
  await expect(connections).toHaveAttribute('tabindex', '-1');
  await expect(page.locator('#sidebar-queries')).toBeVisible();
  await expect(page.locator('#sidebar-connections')).toBeHidden();
  await page.keyboard.press('Home');
  await expect(connections).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(queries).toBeFocused();
  await page.keyboard.press('End');
  await expect(queries).toBeFocused();
});

test('Linux shortcuts advertise Ctrl in the search and save controls', async ({ page }) => {
  test.skip(process.platform === 'darwin', 'Linux/Windows modifier labels');
  await expect(page.getByRole('button', { name: /Quick search/ })).toContainText('Ctrl+K');
  await expect(page.getByRole('button', { name: 'Save query', exact: true })).toHaveAttribute('title', /Ctrl\+S/);
  await page.keyboard.press('Control+k');
  const palette = page.getByRole('dialog', { name: 'Command palette' });
  await expect(palette).toContainText('Ctrl+Enter');
  await page.keyboard.press('Escape');
  await expect(palette).toBeHidden();
});

for (const dark of [false, true]) {
  test(`results remain readable and resizable in a short ${dark ? 'dark' : 'light'} window`, async ({ page }) => {
    if (dark) await page.getByTitle('Toggle theme', { exact: true }).click();
    await runQuery(page);
    await page.setViewportSize({ width: 512, height: 275 });
    const viewport = page.locator('#query-results table').locator('..');
    const assertGridFits = async () => {
      await expect.poll(async () => (await viewport.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(58);
      const box = await viewport.boundingBox();
      expect(box!.y + box!.height).toBeLessThanOrEqual(275);
    };
    await assertGridFits();
    await page.getByRole('button', { name: 'Move split up' }).click();
    await assertGridFits();
    await page.getByRole('button', { name: 'Move split down' }).click();
    await assertGridFits();
    await page.getByRole('separator', { name: 'Resize editor' }).focus();
    await page.keyboard.press('ArrowDown');
    await assertGridFits();
    await page.getByRole('button', { name: 'Results', exact: true }).click();
    await expect(page.locator('#query-results')).toBeHidden();
    await page.getByRole('button', { name: 'Results', exact: true }).click();
    await assertGridFits();
    await page.setViewportSize({ width: 1280, height: 800 });
    await expect.poll(async () => (await viewport.boundingBox())?.height ?? 0).toBeGreaterThan(100);
  });

  test(`audit entries stay visible below compact filters in ${dark ? 'dark' : 'light'} mode`, async ({ page }) => {
    if (dark) await page.getByTitle('Toggle theme', { exact: true }).click();
    await runQuery(page);
    await page.getByRole('button', { name: 'Audit log', exact: true }).click();
    await page.setViewportSize({ width: 512, height: 275 });
    const row = page.getByRole('button', { name: /Show query details for/ }).first();
    await expect(row).toBeInViewport({ ratio: 1 });
    await expect.poll(() => row.evaluate((button) => {
      const scroller = button.closest('.overflow-auto')!;
      const bounds = scroller.getBoundingClientRect();
      const entry = button.closest('tr')!.getBoundingClientRect();
      const top = bounds.top + scroller.clientTop;
      return entry.top >= top && entry.bottom <= top + scroller.clientHeight;
    })).toBe(true);
    await row.click();
    await expect(page.getByText('SELECT * FROM customers;', { exact: true }).last()).toBeVisible();
    await page.getByRole('textbox', { name: 'Search SQL or connection' }).fill('no matching query');
    await expect(page.getByText('No matching entries', { exact: true })).toBeVisible();
  });
}

test('long chart titles wrap outside the plot without overlapping its legend', async ({ page }) => {
  const title = 'Monthly revenue and operating expenses across every customer region during the current reporting year';
  await page.evaluate((title) => {
    localStorage.setItem('sandb.chat.v1', JSON.stringify([{
      id: 'chart-title-test', title: 'Chart title regression', updated: Date.now(),
      sources: [{ connId: 'prod', database: 'analytics_prod' }],
      messages: [{ role: 'assistant', parts: [{
        kind: 'chart', truncated: false,
        spec: { type: 'bar', title, x: 'month', y: ['revenue', 'expenses'], stacked: false },
        columns: ['month', 'revenue', 'expenses'], rows: [['Jan', 100, 50], ['Feb', 120, 60]],
      }] }],
    }]));
  }, title);
  await page.reload();
  const caption = page.locator('figure figcaption').filter({ hasText: title });
  await expect(caption).toBeVisible();
  expect((await caption.boundingBox())!.height).toBeGreaterThan(20);
  expect(await caption.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  const plot = page.getByRole('img', { name: `Chart: ${title}`, exact: true });
  expect((await caption.boundingBox())!.y + (await caption.boundingBox())!.height).toBeLessThanOrEqual((await plot.boundingBox())!.y);
  await expect(plot.locator('svg')).toBeVisible();
  await expect(plot).toContainText('revenue');
  await expect(plot).not.toContainText(title);
  const downloaded = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export as PDF', exact: true }).click();
  const download = await downloaded;
  expect(await download.failure()).toBeNull();
  const pdf = (await readFile((await download.path())!)).toString('latin1');
  const titleLines = [...pdf.matchAll(/\(([^)]*)\) Tj/g)]
    .map((match) => match[1].trim())
    .filter((line) => line.length > 12 && title.includes(line));
  expect(titleLines.length).toBeGreaterThan(1);
  expect(titleLines.join(' ')).toContain(title);
});

test('chat activity shows thinking and database calls in order without hiding the final answer', async ({ page }) => {
  // The demo reply runs on timers (about 2.5 s in total). A paused clock holds it open, so the
  // checks of the live state do not race the reply on a slow or busy machine. The fake clock is
  // installed before the page's scripts run (installed into a running page, it mixes with timers
  // the page already started); it runs normally until the pause right before Send.
  await page.clock.install();
  await page.reload();
  await page.getByPlaceholder('Ask about your data…').fill('Show rows from table customers');
  const send = page.getByRole('button', { name: 'Send', exact: true });
  await expect(send).toBeEnabled();
  await page.clock.pauseAt(Date.now() + 1_000);
  await send.click();
  const toggle = page.getByRole('button', { name: /^Activity/ });
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await page.clock.runFor(1_000);
  await expect(page.getByText('I’ll check the table schema first.', { exact: true })).toBeVisible();
  await page.clock.resume();

  const answer = page.getByText(/Here are the first \d+ rows of customers/);
  await expect(answer).toBeVisible();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('button', { name: /Describe table/ })).toBeHidden();

  await toggle.click();
  await expect(toggle.locator('..')).toContainText(/check the table schema first.*Describe table.*read a small sample.*Read query.*sample is ready to summarize/s);
  await expect(answer).toBeVisible();
  await toggle.click();
  await expect(page.getByText('The sample is ready to summarize.', { exact: true })).toBeHidden();
  await expect(answer).toBeVisible();
});
