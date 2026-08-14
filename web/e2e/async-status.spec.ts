import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

test("late file creation cannot overwrite another project's status", async ({ page }) => {
  await signIn(page, USERS.author)
  const projectA = await createProject(page, "Scoped Create A")
  await page.locator("#go-projects").click()
  const projectB = await createProject(page, "Scoped Create B")
  await page.locator("#go-projects").click()
  const projectAButton = page.locator("[data-project]", { hasText: projectA })
  const projectAId = await projectAButton.getAttribute("data-project")
  expect(projectAId).toBeTruthy()
  await projectAButton.click()
  await page.locator("[data-document]").first().click()

  await page.route(`**/projects/${projectAId}/documents`, async (route) => {
    if (route.request().method() !== "POST") return route.continue()
    const response = await route.fetch()
    await new Promise((resolve) => setTimeout(resolve, 1_500))
    await route.fulfill({ response })
  })
  await page.locator("#add-document").click()
  await page.locator("#prompt-input").fill("late-a.typ")
  await page.locator("#prompt-ok").click()
  await page.locator("#go-projects").click()
  await page.locator("[data-project]", { hasText: projectB }).click()
  await page.locator("[data-document]").first().click()

  await page.waitForTimeout(2_000)
  await expect(page.locator("#save-status")).not.toHaveText("File created")
  await expect(page.locator("[data-document]")).toHaveCount(1)
})
