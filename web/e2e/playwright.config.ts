import { defineConfig } from "@playwright/test"

/**
 * Playwright E2E tests run against the full Docker Compose stack.
 * The stack must be up (just up-all) before running these tests.
 *
 * Run: bunx playwright test --config e2e/
 * Or:  bun run --cwd web e2e
 */
export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.ts",
  timeout: 30_000,
  retries: 0,
  workers: 1, // Serial: tests share state (projects, documents)
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://127.0.0.1:8103",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    actionTimeout: 10_000,
  },
})
