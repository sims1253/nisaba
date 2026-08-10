
/**
 * Sharing and membership tests: verify the share panel works and members can be
 * removed again (regression for the missing member-removal affordance, 2026-08-09).
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject } from "./helpers"

test.describe("Sharing and membership", () => {
  test("share panel opens and accepts username", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Shared Project Test")

    const shareBtn = page.locator("#share-button")
    await shareBtn.waitFor({ state: "visible", timeout: 10_000 })
    await shareBtn.click()

    const shareInput = page.locator("#share-subject")
    await shareInput.waitFor({ state: "visible", timeout: 10_000 })
    await shareInput.fill("reviewer")
    await expect(shareInput).toHaveValue("reviewer")
  })

  test("owner can remove a member from the share panel", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Member Remove Test")
    await page.locator("#share-button").waitFor({ state: "visible", timeout: 10_000 })
    await page.locator("#share-button").click()
    await page.locator("#share-subject").waitFor({ state: "visible", timeout: 10_000 })

    // Invite two members so the list has removable rows.
    for (const subject of ["reviewer", "reader"]) {
      await page.locator("#share-subject").fill(subject)
      await page.locator("#share-invite").click()
      await page.waitForTimeout(800)
    }
    // Both appear in the members list with Remove buttons (owner row has none).
    await page.waitForSelector('[data-remove-member="reviewer"]', { state: "visible", timeout: 10_000 })
    await page.waitForSelector('[data-remove-member="reader"]', { state: "visible", timeout: 10_000 })
    await expect(page.locator('[data-remove-member="demo"]')).toHaveCount(0)

    // Remove the reviewer: the row disappears from the members list.
    await page.locator('[data-remove-member="reviewer"]').click()
    await page.waitForTimeout(1500)
    await expect(page.locator('[data-remove-member="reviewer"]')).toHaveCount(0)
    await expect(page.locator('[data-remove-member="reader"]')).toHaveCount(1)
  })
})
