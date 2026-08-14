import { test, expect, type Page } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

async function findAndClose(page: Page, query: string): Promise<string> {
  await page.keyboard.press("Control+f")
  const search = page.locator(".cm-search .cm-textfield").first()
  await expect(search).toBeVisible()
  await search.fill(query)
  await page.waitForTimeout(300)
  await search.press("Enter")
  await page.keyboard.press("Escape")
  return page.evaluate(() => getSelection()?.toString() ?? "")
}

test("pasting a new Find query selects that query rather than the previous one", async ({ page }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Find Current Query")
  const editor = page.locator(".cm-content")
  await editor.click()
  await page.keyboard.type("ANCHOR-ONE alpha\nANCHOR-TWO beta\nANCHOR-THREE gamma")
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })

  await findAndClose(page, "ANCHOR-ONE")
  expect(await findAndClose(page, "ANCHOR-TWO")).toBe("ANCHOR-TWO")
  expect(await findAndClose(page, "ANCHOR-THREE")).toBe("ANCHOR-THREE")
})
