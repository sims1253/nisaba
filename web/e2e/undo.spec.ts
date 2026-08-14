import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

for (const redoKey of ["Control+Shift+z", "Control+y"] as const) {
  test(`${redoKey} restores one undone edit without corrupting the baseline`, async ({ page }) => {
    await signIn(page, USERS.author)
    await createProject(page, `Undo ${redoKey}`)
    const baseline = `BASELINE-${Date.now()}`
    const marker = `-REDO-${redoKey}`
    const editor = page.locator(".cm-content")

    await editor.click()
    await page.keyboard.type(baseline)
    await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
    await page.keyboard.type(marker)
    await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })

    await page.keyboard.press("Control+z")
    await expect(editor).toContainText(baseline)
    await expect(editor).not.toContainText(marker)
    await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })

    await page.keyboard.press(redoKey)
    await expect(editor).toContainText(`${baseline}${marker}`)
    await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
    await page.reload()
    await expect(editor).toContainText(`${baseline}${marker}`, { timeout: 15_000 })
  })
}
