import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'ui.spec.mjs',
  timeout: 90_000,
  workers: 1,
  retries: 0,
  use: { baseURL: 'http://127.0.0.1:8787', headless: true },
});
