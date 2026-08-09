/**
 * Sharing and membership tests: invite a user and verify they can access the project.
 *
 * This test catches BUG-04 from the 2026-08-09 evaluation: the sharing UI
 * stored the username but the auth middleware matched against the OIDC `sub`
 * (UUID), so invited users could never access the project.
 */

import { test, expect, type Browser } from "@playwright/test"
import { signIn, createProject, BASE_URL } from "./helpers"

test.describe("Sharing and membership", () => {
  test("invited user can access the shared project", async ({ browser }: { browser: Browser }) => {
    // Author creates a project
    const authorCtx = await browser.newContext()
    const authorPage = await authorCtx.newPage()
    await signIn(authorPage, { username: "demo", password: "demo", role: "author" })
    await createProject(authorPage, "Shared Project Test")

    const projectUrl = authorPage.url()

    // Invite the reviewer through the Share panel
    await authorPage.getByRole("button", { name: /share/i }).click()
    await authorPage.getByPlaceholder("Username to invite").fill("reviewer")
    // Select reviewer role
    const roleSelect = authorPage.getByRole("combobox").or(authorPage.locator("select"))
    if (await roleSelect.isVisible()) {
      await roleSelect.selectOption("reviewer")
    }
    await authorPage.getByRole("button", { name: /invite|add/i }).click()
    await authorPage.waitForTimeout(2000)

    // Verify the member appears in the list
    const memberList = authorPage.locator(".reference-item, .member-item, [data-member]")
    const memberText = await memberList.allTextContents()
    expect(memberText.some((t) => t.includes("reviewer"))).toBe(true)

    // Close share panel
    await authorPage.keyboard.press("Escape")
    await authorCtx.close()

    // Reviewer signs in and should see the project
    const reviewerCtx = await browser.newContext()
    const reviewerPage = await reviewerCtx.newPage()
    await signIn(reviewerPage, { username: "reviewer", password: "reviewer", role: "reviewer" })

    // Navigate directly to the project
    await reviewerPage.goto(projectUrl)
    await reviewerPage.waitForTimeout(3000)

    // The reviewer should NOT get a 403/forbidden error
    // Check that the project content is visible (not an error state)
    const errorText = reviewerPage.locator("text=/forbidden|403|access denied/i")
    await expect(errorText).toHaveCount(0)

    // The editor or document content should be visible
    const editor = reviewerPage.locator(".cm-content, #document-name, .editor")
    await expect(editor.first()).toBeVisible({ timeout: 10_000 })

    await reviewerCtx.close()
  })
})
