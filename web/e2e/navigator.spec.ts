import { test, expect } from "@playwright/test"
import { createDocument, createProject, signIn, USERS } from "./helpers"

test("deep outline navigation does not cover the Files controls", async ({ page }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Navigator Independent Scrollers")
  await createDocument(page, "aux.typ", "Aux")

  const main = page.locator("[data-document]", { hasText: "main.typ" })
  await main.click()
  const body = Array.from({ length: 80 }, (_, index) =>
    `= Navigator heading ${index + 1}\n\nBody ${index + 1}.\n`,
  ).join("\n")
  const editor = page.locator(".cm-content")
  await editor.fill(body)
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })

  const lastHeading = page.locator("#section-outline .outline-row", { hasText: "Navigator heading 80" })
  await expect(lastHeading).toBeVisible()
  await lastHeading.click()

  for (const selector of ["#add-document", '[data-document]:has-text("aux.typ")']) {
    const hitTarget = await page.locator(selector).evaluate((element) => {
      const rect = element.getBoundingClientRect()
      const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2)
      return {
        expected: element.id || element.getAttribute("data-document"),
        actual: hit?.id || hit?.closest("[data-document]")?.getAttribute("data-document") || hit?.tagName,
        contains: element === hit || element.contains(hit),
      }
    })
    expect(hitTarget, `${selector} must remain the top pointer target`).toMatchObject({ contains: true })
  }

  await page.locator("#add-document").click()
  await expect(page.locator("#prompt-input")).toBeVisible()
  await page.locator("#prompt-cancel").click()
  await page.locator('[data-document]:has-text("aux.typ")').click()
  await expect(page.locator("#document-path")).toContainText("aux.typ")
})
