/**
 * Permission boundary tests: role-based UI gates.
 *
 * Regression coverage for the 2026-08-09 reviewer-write fix AND the follow-up
 * role-gate fix: the gates used to target non-existent selectors
 * (#add-document-btn, .delete-document-btn, #add-reference-btn), so the tests
 * passed vacuously while reviewers/read-only users SAW the destructive
 * controls. These tests assert the REAL rendered DOM
 * (#add-document, [data-delete-document], [data-delete-project], #add-reference,
 * #new-project) — a selector typo now fails loudly.
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject } from "./helpers"

const HIDDEN = { timeout: 5_000 }

test.describe("Role UI gates (real DOM selectors)", () => {
  test("read-only user does not see author-only controls", async ({ browser }) => {
    const owner = await browser.newContext()
    const ownerPage = await owner.newPage()
    await signIn(ownerPage, { username: "demo", password: "demo", role: "author" })
    await createProject(ownerPage, "Reader Gate Test")
    await ownerPage.locator("#share-button").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-button").click()
    await ownerPage.locator("#share-subject").waitFor({ state: "visible", timeout: 10_000 })
    await ownerPage.locator("#share-subject").fill("reader")
    await ownerPage.locator("#share-role").selectOption("read-only")
    await ownerPage.locator("#share-invite").click()
    await ownerPage.waitForTimeout(1500)

    const reader = await browser.newContext()
    const page = await reader.newPage()
    await signIn(page, { username: "reader", password: "reader", role: "read-only" })
    // At the project list: no ＋, no delete on any project row.
    await expect(page.locator("#new-project")).toBeHidden(HIDDEN)
    const deleteProjectRows = page.locator("[data-delete-project]")
    const rowCount = await deleteProjectRows.count()
    for (let i = 0; i < rowCount; i++) {
      await expect(deleteProjectRows.nth(i)).toBeHidden(HIDDEN)
    }
    // Open the invited project and a document (the editor is read-only but
    // still renders).
    await page.locator("[data-project]", { hasText: "Reader Gate Test" }).first().click()
    await page.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-document]").first().click()
    await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })
    // Inside: no Add document, no Add demo, no row deletes, no Add reference,
    // no Share/Export; the Compile button must be ENABLED (read-only users can
    // compile per the user-guide roles table).
    await expect(page.locator("#add-document")).toBeHidden(HIDDEN)
    await expect(page.locator("#add-demo")).toBeHidden(HIDDEN)
    await expect(page.locator("#add-reference")).toBeHidden(HIDDEN)
    await expect(page.locator("#share-button")).toBeHidden(HIDDEN)
    await expect(page.locator("#export-button")).toBeHidden(HIDDEN)
    const deleteDocs = page.locator("[data-delete-document]")
    const docCount = await deleteDocs.count()
    for (let i = 0; i < docCount; i++) {
      await expect(deleteDocs.nth(i)).toBeHidden(HIDDEN)
    }
    await expect(page.locator("#compile-button")).toBeEnabled()

    await owner.close()
    await reader.close()
  })

  test("reviewer does not see create/delete controls (they 403 server-side)", async ({ browser }) => {
    const owner = await browser.newContext()
    const ownerPage = await owner.newPage()
    await signIn(ownerPage, { username: "demo", password: "demo", role: "author" })
    await createProject(ownerPage, "Reviewer Gate Test")
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
    await page.locator("[data-project]").first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-project]", { hasText: "Reviewer Gate Test" }).first().click()
    await page.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
    await page.locator("[data-document]").first().click()
    await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })

    // No Add Document / Add demo / Add reference for a reviewer.
    await expect(page.locator("#add-document")).toBeHidden(HIDDEN)
    await expect(page.locator("#add-demo")).toBeHidden(HIDDEN)
    await expect(page.locator("#add-reference")).toBeHidden(HIDDEN)
    // No delete buttons on the document rows.
    const deleteBtns = page.locator("[data-delete-document]")
    const count = await deleteBtns.count()
    for (let i = 0; i < count; i++) {
      await expect(deleteBtns.nth(i)).toBeHidden(HIDDEN)
    }
    // Share button (member management) is owner/author-only too; Compile stays
    // enabled (reviewers can compile).
    await expect(page.locator("#share-button")).toBeHidden(HIDDEN)
    await expect(page.locator("#export-button")).toBeHidden(HIDDEN)
    await expect(page.locator("#compile-button")).toBeEnabled()

    await owner.close()
    await reviewer.close()
  })
})
