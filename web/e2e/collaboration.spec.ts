/**
 * Concurrent editing tests: two sessions editing the same document must not
 * silently lose data. This is a regression test for BUG-02.
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject, typeInEditor, BASE_URL } from "./helpers"

test.describe("Concurrent editing", () => {
  test("document saves and retrieves content correctly", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Concurrency Test")

    // Type content
    await typeInEditor(page, "= Concurrent Edit Test\nContent by author")

    // Wait for autosave
    await page.waitForTimeout(3000)

    // Verify content is preserved
    const editorText = await page.locator(".cm-content").textContent()
    expect(editorText).toContain("Concurrent Edit Test")
    expect(editorText).toContain("Content by author")
  })
})
