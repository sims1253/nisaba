/**
 * Permission boundary tests: read-only users must NOT see enabled write controls.
 *
 * This test catches BUG-06 from the 2026-08-09 evaluation: read-only users
 * could see and interact with delete, compile, and edit controls, even though
 * the backend correctly returned 403. The UI should gate these up-front.
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject, USERS, BASE_URL } from "./helpers"

test.describe("Read-only role UI gates", () => {
  test("read-only user cannot see destructive controls", async ({ browser }) => {
    // First: author creates a project and shares it with the reader
    const authorPage = await browser.newPage()
    await signIn(authorPage, USERS.author)
    await createProject(authorPage, "Permission Test Project")

    // Get project ID from URL
    const projectId = authorPage.url().split("/projects/")[1]?.split("/")[0]
    expect(projectId).toBeTruthy()

    // Share with reader via UI
    await authorPage.getByRole("button", { name: /share/i }).click()
    await authorPage.getByPlaceholder("Username to invite").fill("reader")
    await authorPage.getByRole("button", { name: /invite|add/i }).click()
    // Close share panel
    await authorPage.keyboard.press("Escape")
    await authorPage.context().close()

    // Now: reader signs in and opens the project
    const readerPage = await browser.newPage()
    await signIn(readerPage, USERS.reader)
    await readerPage.goto(`${BASE_URL}/#/projects/${projectId}`)
    await readerPage.waitForTimeout(2000)

    // Delete buttons must be hidden or disabled
    const deleteBtns = readerPage.locator("button:has-text('Delete')")
    await expect(deleteBtns).toHaveCount(0)

    // Compile button should be disabled for read-only
    const compileBtn = readerPage.locator("#compile-button")
    if (await compileBtn.isVisible()) {
      await expect(compileBtn).toBeDisabled()
    }

    // Share button should be hidden
    const shareBtn = readerPage.locator("#share-button")
    if (await shareBtn.isVisible()) {
      await expect(shareBtn).toBeHidden()
    }

    // Editor should be read-only
    const editor = readerPage.locator(".cm-content")
    const isEditable = await editor.getAttribute("contenteditable")
    // The editor should not accept input
    expect(isEditable === "true").toBe(false)

    await readerPage.context().close()
  })
})
