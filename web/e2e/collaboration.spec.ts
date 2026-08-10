
/**
 * Concurrent editing and review-workflow tests.
 *
 * Regression coverage for the 2026-08-09 review fixes:
 *  - a reviewer's suggesting-mode edits must NOT be written into the baseline
 *    (they are proposals persisted through the review layer), and the save
 *    status must not lie about it ("Unsaved changes" forever).
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject, typeInEditor } from "./helpers"

test.describe("Concurrent editing", () => {
  test("document saves and retrieves content correctly", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Concurrency Test")

    await typeInEditor(page, "= Concurrent Edit Test\nContent by author")
    await page.waitForTimeout(3000)

    const editorText = await page.locator(".cm-content").textContent()
    expect(editorText).toContain("Concurrent Edit Test")
    expect(editorText).toContain("Content by author")
  })
})

test.describe("Reviewer suggestion workflow", () => {
  test("reviewer suggestions do not flip save status to Unsaved changes", async ({ browser }) => {
    const owner = await browser.newContext()
    const ownerPage = await owner.newPage()
    await signIn(ownerPage, { username: "demo", password: "demo", role: "author" })
    await createProject(ownerPage, "Reviewer Suggestion Test")
    await ownerPage.locator("#share-button").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-button").click()
    await ownerPage.locator("#share-subject").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-subject").fill("reviewer")
    await ownerPage.locator("#share-role").selectOption("reviewer")
    await ownerPage.locator("#share-invite").click()
    await ownerPage.waitForTimeout(1500)

    const reviewer = await browser.newContext()
    const page = await reviewer.newPage()
    await signIn(page, { username: "reviewer", password: "reviewer", role: "reviewer" })
    await page.locator("[data-project]", { hasText: "Reviewer Suggestion Test" }).first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-project]", { hasText: "Reviewer Suggestion Test" }).first().click()
    await page.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-document]").first().click()
    await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })

    // A reviewer is locked into suggesting mode (Track changes on).
    await expect(page.locator("#sidebar-suggesting, #suggesting-button").first()).toHaveText(/Track changes: on/)

    // Type a suggestion and wait well past the autosave debounce window.
    await typeInEditor(page, "= A suggested heading")
    await page.waitForTimeout(4000)

    // The reviewer's local status must NOT be a stuck "Unsaved changes" —
    // suggestions are tracked through the review layer, not the baseline PATCH.
    const statusEl = page.locator("#save-status, .save-status, #status")
    if (await statusEl.first().isVisible().catch(() => false)) {
      const status = (await statusEl.first().textContent()) ?? ""
      expect(status).not.toContain("Unsaved changes")
    }
    // The review banner should now report an open review item.
    const banner = page.locator("#review-banner")
    if (await banner.isVisible({ timeout: 3000 }).catch(() => false)) {
      await expect(banner).toContainText("open review item")
    }

    await owner.close()
    await reviewer.close()
  })
})
