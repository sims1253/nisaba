
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
    const projectName = await createProject(ownerPage, "Reviewer Suggestion Test")
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
    await page.locator("[data-project]", { hasText: projectName }).waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-project]", { hasText: projectName }).click()
    await page.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-document]").first().click()
    await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })

    // A reviewer is locked into suggesting mode (Track changes on).
    await expect(page.locator("#sidebar-suggesting, #suggesting-button").first()).toHaveText(/Track changes: on/)

    // Type a suggestion and wait well past the autosave debounce window.
    await typeInEditor(page, "= A suggested heading")
    await page.waitForTimeout(4000)

    // Text and its review record must reach the relay atomically. Sending the
    // text commit first makes the reviewer policy reject the update (4003),
    // disconnects sync, and loses the suggestion on reload.
    await expect(page.locator("#sync-label")).toContainText("Live")

    // The reviewer's local status must NOT be a stuck "Unsaved changes" —
    // suggestions are tracked through the review layer, not the baseline PATCH.
    const statusEl = page.locator("#save-status, .save-status, #status")
    if (await statusEl.first().isVisible().catch(() => false)) {
      const status = (await statusEl.first().textContent()) ?? ""
      expect(status).not.toContain("Unsaved changes")
    }
    // The suggestion must show up as an open review item. The old amber banner
    // that used to carry this count is gone (it duplicated the Review button and
    // the queue); the count now lives on the Review button only, so that is what
    // this asserts.
    await expect(page.locator("#review-count")).toHaveText(/[1-9]/, { timeout: 10_000 })
    await page.locator("#review-button").click()
    await expect(page.locator(".review-card").first()).toBeVisible({ timeout: 10_000 })

    await page.reload()
    await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible({ timeout: 15_000 })
    // Reload restores the last open project when session state is available;
    // only navigate from the project screen when the editor was not restored.
    const restoredProject = page.locator("[data-project]", { hasText: projectName })
    if (await restoredProject.isVisible()) await restoredProject.click()
    if (!await page.locator(".cm-content").isVisible()) {
      await page.locator("[data-document]").first().click()
      await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })
    }
    await page.locator("#review-button").click()
    await expect(page.locator(".review-card").filter({ hasText: "A suggested heading" })).toBeVisible({ timeout: 15_000 })

    // The author sees proposed text live, but an unresolved reviewer proposal
    // must not be autosaved into the authoritative REST baseline.
    await expect(ownerPage.locator("#review-count")).toHaveText(/[1-9]/, { timeout: 10_000 })
    const documentId = await ownerPage.locator("[data-document]").first().getAttribute("data-document")
    await ownerPage.locator("#go-projects").click()
    const projectId = await ownerPage.locator("[data-project]", { hasText: projectName }).getAttribute("data-project")
    const baseline = await ownerPage.evaluate(async ({ projectId, documentId }) => {
      const stored = JSON.parse(localStorage.getItem("nisaba.auth.token") ?? "{}") as { accessToken?: string }
      const response = await fetch(`/api/projects/${projectId}/documents/${documentId}`, {
        headers: { authorization: `Bearer ${stored.accessToken ?? ""}` }
      })
      return response.json() as Promise<{ body: string }>
    }, { projectId, documentId })
    expect(baseline.body).not.toContain("A suggested heading")

    await owner.close()
    await reviewer.close()
  })
})
