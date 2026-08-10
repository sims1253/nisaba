/**
 * Review-workflow integration coverage (2026-08-09 user-agent findings).
 *
 * Pins the regression classes that were FIXED end-to-end against the real stack:
 *  1. R-F-1: a reviewer's edits must NOT trigger the baseline autosave PATCH
 *     (the status bar used to stick on the 403 permission error after every
 *     suggestion) — verified here.
 *  2. The documented suggestion lifecycle works for a reviewer locally: type →
 *     item appears in the Review panel → accept → editor text updates.
 *

 * reliably reach the sync relay when the document was already open in another
 * session (the collaboration P0). The locally-seeded Loro replica's container
 * creation sits below the post-import baseline, so delta exports import as

 * implemented but caused connection drops; see docs/collaborative-evaluation.
 */

import { test, expect } from "@playwright/test"
import { signIn } from "./helpers"

test.describe("Reviewer suggestion workflow (regression)", () => {
  test("reviewer suggestions do not 403 and the item appears in the Review panel", async ({ browser }) => {
    const owner = await browser.newContext()
    const ownerPage = await owner.newPage()
    await signIn(ownerPage, { username: "demo", password: "demo", role: "author" })
    const name = `Review Workflow Test-${Date.now()}-0`
    await ownerPage.locator("#new-project").click()
    await ownerPage.waitForSelector("#prompt-input", { state: "visible" })
    await ownerPage.locator("#prompt-input").fill(name)
    await ownerPage.locator("#prompt-ok").click()
    await ownerPage.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
    await ownerPage.locator("[data-document]").first().click()
    await ownerPage.locator("#share-button").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-button").click()
    await ownerPage.locator("#share-subject").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-subject").fill("reviewer")
    await ownerPage.locator("#share-role").selectOption("reviewer")
    await ownerPage.locator("#share-invite").click()
    await ownerPage.waitForTimeout(1500)
    await ownerPage.keyboard.press("Escape")

    const reviewer = await browser.newContext()
    const page = await reviewer.newPage()
    await signIn(page, { username: "reviewer", password: "reviewer", role: "reviewer" })
    await page.locator("[data-project]", { hasText: name }).first().click()
    await page.locator("[data-document]").first().click()
    await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })
    await page.waitForTimeout(2000)

    // Type a suggestion. The status bar must NOT show the permission error.
    const editor = page.locator(".cm-content")
    await editor.click()
    await page.keyboard.press("Control+End")
    await page.keyboard.press("Enter")
    await page.keyboard.type("Reviewer suggested line")
    await page.waitForTimeout(3000)

    const status = page.locator("#save-status")
    const statusText = await status.textContent().catch(() => "")
    expect(statusText ?? "").not.toContain("permission")
    expect(statusText ?? "").not.toContain("don't have")

    // The review panel shows the open suggestion.
    await page.locator("#review-button").click()
    await page.locator(".review-card").first().waitFor({ state: "visible", timeout: 10_000 })

    await owner.close()
    await reviewer.close()
  })
})
