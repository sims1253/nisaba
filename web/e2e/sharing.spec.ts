/**
 * Sharing and membership tests: verify the share panel works.
 * Regression test for BUG-04.
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject } from "./helpers"

test.describe("Sharing and membership", () => {
  test("share panel opens and accepts username", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Shared Project Test")

    // Open the share panel
    const shareBtn = page.locator("#share-button")
    await shareBtn.waitFor({ state: "visible", timeout: 10_000 })
    await shareBtn.click()

    // The share panel should show a username input
    const shareInput = page.locator("#share-subject")
    await shareInput.waitFor({ state: "visible", timeout: 10_000 })

    // Type a username and verify the input accepts it
    await shareInput.fill("reviewer")
    await expect(shareInput).toHaveValue("reviewer")
  })
})
