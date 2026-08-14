import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

test("concurrent deletion of the same document reconciles both tabs", async ({ browser }) => {
  const contextA = await browser.newContext()
  const contextB = await browser.newContext()
  const pageA = await contextA.newPage()
  const pageB = await contextB.newPage()
  await signIn(pageA, USERS.author)
  const projectName = await createProject(pageA, "Concurrent Document Delete")
  await signIn(pageB, USERS.author)
  await pageB.getByText(projectName, { exact: true }).click()
  await pageB.locator("[data-document]").first().click()
  await expect(pageB.locator(".cm-content")).toBeVisible()

  await Promise.all([
    pageA.locator("[data-delete-document]").first().click(),
    pageB.locator("[data-delete-document]").first().click(),
  ])
  await Promise.all([
    pageA.locator("#prompt-input").fill("Main"),
    pageB.locator("#prompt-input").fill("Main"),
  ])
  await Promise.all([
    pageA.locator("#prompt-ok").click(),
    pageB.locator("#prompt-ok").click(),
  ])

  for (const page of [pageA, pageB]) {
    await expect(page.locator("[data-document]")).toHaveCount(0, { timeout: 10_000 })
    await expect(page.locator(".cm-editor")).toBeHidden()
    await expect(page.locator("#save-status")).not.toContainText("not found")
    await expect(page.locator("#save-status")).not.toContainText("access revoked")
  }

  await contextA.close()
  await contextB.close()
})
