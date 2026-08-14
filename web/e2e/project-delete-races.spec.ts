import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

test("concurrent deletion of the same project reconciles both tabs", async ({ browser }) => {
  const contextA = await browser.newContext()
  const contextB = await browser.newContext()
  const pageA = await contextA.newPage()
  const pageB = await contextB.newPage()
  await signIn(pageA, USERS.author)
  const projectName = await createProject(pageA, "Concurrent Project Delete")
  await pageA.locator("#go-projects").click()
  await signIn(pageB, USERS.author)

  for (const page of [pageA, pageB]) {
    const row = page.locator(".project-row", { hasText: projectName })
    await row.locator("[data-delete-project]").click()
    await page.locator("#prompt-input").fill(projectName)
  }
  await Promise.all([
    pageA.locator("#prompt-ok").click(),
    pageB.locator("#prompt-ok").click(),
  ])

  for (const page of [pageA, pageB]) {
    await expect(page.locator(".project-row", { hasText: projectName })).toHaveCount(0, { timeout: 10_000 })
    await expect(page.locator("#save-status")).not.toContainText("permission")
  }
  await contextA.close()
  await contextB.close()
})
