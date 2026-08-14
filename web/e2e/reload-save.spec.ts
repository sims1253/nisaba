import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

test("reload during a delayed autosave restores the newest local body", async ({ page }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Reload Save Recovery")
  const marker = `RELOAD-SAVE-${Date.now()}`
  let delayed = false
  await page.route("**/documents/*", async (route) => {
    if (route.request().method() !== "PATCH" || delayed) return route.continue()
    delayed = true
    await new Promise((resolve) => setTimeout(resolve, 1_800))
    await route.continue()
  })

  await page.locator(".cm-content").fill(marker)
  await expect(page.locator("#save-status")).toHaveText("Unsaved changes")
  page.on("dialog", (dialog) => void dialog.accept())
  await page.reload()

  await expect(page.locator("[data-document]").first()).toBeVisible({ timeout: 10_000 })
  await expect(page.locator(".cm-content")).toContainText(marker, { timeout: 12_000 })
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 12_000 })
})
