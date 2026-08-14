import { test, expect } from "@playwright/test"
import { createDocument, createProject, signIn, USERS } from "./helpers"

test("failed offline draft blocks navigation and retries after reconnect", async ({ page, context }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Offline Draft Navigation")
  await createDocument(page, "second.typ", "Second")
  await page.locator('[data-document]:has-text("main.typ")').click()
  const marker = `OFFLINE-DRAFT-${Date.now()}`

  await context.setOffline(true)
  await page.locator(".cm-content").fill(marker)
  await expect(page.locator("#save-status")).toHaveText("Failed to fetch", { timeout: 10_000 })

  await page.locator('[data-document]:has-text("second.typ")').click()
  await expect(page.locator("#document-path")).toContainText("main.typ")
  await expect(page.locator("#save-status")).toContainText("reconnect before switching")
  await page.locator("#go-projects").click()
  await expect(page.locator("#workspace")).toBeVisible()
  await expect(page.locator("#save-status")).toContainText("reconnect before leaving")

  await context.setOffline(false)
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 12_000 })
  await expect(page.locator(".cm-content")).toContainText(marker)
  await page.locator('[data-document]:has-text("second.typ")').click()
  await page.locator('[data-document]:has-text("main.typ")').click()
  await expect(page.locator(".cm-content")).toContainText(marker)
})
