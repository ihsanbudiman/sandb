import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './testdb/ui',
  use: { baseURL: 'http://localhost:5299', browserName: 'chromium' },
  webServer: {
    command: 'SANDB_TEST_FOREGROUND=1 scripts/test-env.sh mock',
    url: 'http://localhost:5299',
    reuseExistingServer: false,
  },
});
