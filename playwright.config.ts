import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './testdb/ui',
  use: { baseURL: 'http://localhost:5299', browserName: 'chromium' },
  webServer: {
    command: 'pnpm test-env mock',
    url: 'http://localhost:5299',
    reuseExistingServer: !process.env.CI,
  },
});
