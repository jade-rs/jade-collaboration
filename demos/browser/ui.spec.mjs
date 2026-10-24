import { test, expect } from '@playwright/test';

test('actual editors preserve an IME composition through peer edits and reconnect', async ({page}) => {
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto('/browser-tests.html');
  await page.getByRole('button',{name:'テストを実行',exact:true}).click();
  await expect(page.locator('#result')).toHaveAttribute('data-status',/passed|failed/,{timeout:70_000});
  const result = await page.locator('#result').textContent();
  expect(result).toContain('"status": "passed"');
  expect(result).toContain('final-input-before-compositionend');
  expect(result).toContain('final-input-after-compositionend');
  expect(errors).toEqual([]);
});
