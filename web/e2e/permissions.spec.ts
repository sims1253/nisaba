/**
 * Permission boundary tests: read-only users must NOT see enabled write controls.
 * Regression test for BUG-06.
 */

import { test, expect } from "@playwright/test"
import { signIn, BASE_URL } from "./helpers"

test.describe("Read-only role UI gates", () => {
  test("read-only user sees sign-in and app shell", async ({ page }) => {
    await signIn(page, { username: "reader", password: "reader", role: "read-only" })

    // The reader should be signed in
    await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible()

    // Share button should be hidden for read-only
    const shareBtn = page.locator("#share-button")
    if (await shareBtn.isVisible({ timeout: 2000 }).catch(() => false)) {
      await expect(shareBtn).toBeHidden()
    }

    // Export button should be hidden for read-only
    const exportBtn = page.locator("#export-button")
    if (await exportBtn.isVisible({ timeout: 2000 }).catch(() => false)) {
      await expect(exportBtn).toBeHidden()
    }
  })
})
