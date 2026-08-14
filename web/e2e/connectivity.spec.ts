import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

test("reconnect clears stale network failure from an offline create action", async ({ page, context }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Offline Action Status")

  await context.setOffline(true)
  await page.locator("#add-document").click()
  await page.locator("#prompt-input").fill("offline-create.typ")
  await page.locator("#prompt-ok").click()
  await expect(page.locator("#save-status")).toHaveText("Failed to fetch", { timeout: 10_000 })

  await context.setOffline(false)
  await expect(page.locator("#save-status")).toHaveText("Back online — retry the action")
  await expect(page.locator("[data-document]", { hasText: "offline-create.typ" })).toHaveCount(0)
})
