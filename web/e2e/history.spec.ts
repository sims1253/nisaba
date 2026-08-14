import { test, expect } from "@playwright/test"
import { createDocument, createProject, signIn, USERS } from "./helpers"

async function replaceEditor(page: import("@playwright/test").Page, body: string): Promise<void> {
  const editor = page.locator(".cm-content")
  await editor.click()
  await page.keyboard.press("Control+A")
  await page.keyboard.type(body)
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
}

test("late history response cannot populate the dock after switching documents", async ({ page }) => {
  await signIn(page, USERS.author)
  await createProject(page, "History Request Identity")

  const mainMarker = `MAIN-HISTORY-${Date.now()}`
  const secondMarker = `SECOND-HISTORY-${Date.now()}`
  await replaceEditor(page, mainMarker)
  await createDocument(page, "second.typ", "Second")

  const main = page.locator("[data-document]", { hasText: "main.typ" })
  const second = page.locator("[data-document]", { hasText: "second.typ" })
  await second.click()
  await replaceEditor(page, secondMarker)
  const secondId = await second.getAttribute("data-document")
  expect(secondId).toBeTruthy()

  await page.route(`**/documents/${secondId}/history`, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1_500))
    await route.continue()
  })

  await page.locator("#history-button").click()
  await main.click()

  // The open dock follows the newly-selected file. The delayed second-file
  // response must neither replace this timeline nor supply its revision bodies.
  await expect(page.locator("#history-timeline")).toBeVisible({ timeout: 10_000 })
  await page.locator(".history-entry").first().click()
  await expect(page.locator("#history-diff-pane")).toContainText(mainMarker)
  await expect(page.locator("#history-diff-pane")).not.toContainText(secondMarker)
})
