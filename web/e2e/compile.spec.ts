/**
 * Compile and PDF preview tests.
 *
 * This test catches BUG-05 from the 2026-08-09 evaluation: the PDF worker
 * module failed to load despite a successful backend compile. The test
 * verifies that clicking Compile renders actual PDF pages in the preview pane.
 */

import { test, expect } from "@playwright/test"
import { signIn, createProject, openFirstProject } from "./helpers"

test.describe("Compile and PDF preview", () => {
  test("compile renders a PDF preview", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Compile Test")
    await openFirstProject(page)

    const editor = page.locator(".cm-content")
    await editor.click()
    await page.keyboard.press("Control+a")
    await page.keyboard.type("#set page(width: 10cm, height: auto)\n= Hello Compile Test\nThis is a test document.")

    // Click compile
    await page.locator("#compile-button").click()

    // Wait for PDF canvas to appear (the pdf.js viewer renders pages as <canvas>)
    await expect(page.locator(".pdf-page canvas").first()).toBeVisible({
      timeout: 30_000,
    })

    // Verify at least one canvas has non-zero dimensions (it rendered)
    const canvas = page.locator(".pdf-page canvas").first()
    const width = await canvas.evaluate((el: HTMLCanvasElement) => el.width)
    expect(width).toBeGreaterThan(0)
  })

  test("compile error is surfaced to the user", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Compile Error Test")
    await openFirstProject(page)

    const editor = page.locator(".cm-content")
    await editor.click()
    await page.keyboard.press("Control+a")
    await page.keyboard.type("#invalid_function_that_does_not_exist()")

    // Click compile — should show an error, not crash
    await page.locator("#compile-button").click()

    // The preview area should show an error state, not a blank page
    await page.waitForTimeout(5000)
    const previewText = await page.locator('[role="region"], .pdf-viewer, #preview').first().textContent()
    // Something should be shown — an error message or diagnostic
    expect(previewText).toBeTruthy()
  })
})
