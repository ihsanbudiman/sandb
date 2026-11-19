import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './testdb/ui',
  use: { baseURL: 'http://localhost:5199', browserName: 'chromium' },
  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:5199',
    reuseExistingServer: !process.env.CI,
  },
});
