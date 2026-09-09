/** Browser coverage for compilation, diagnostics, and PDF preview. */

import { test, expect } from "@playwright/test"
import { signIn, createProject, createDocument, openFirstProject } from "./helpers"

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

  test("preview keeps the project entrypoint while editing a chapter", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Preview Entry Test")
    await createDocument(page, "chapter.typ", "chapter.typ")
    await page.locator("[data-document]").filter({ hasText: "chapter.typ" }).click()
    await expect(page.locator("#document-path")).toHaveText("chapter.typ")
    await expect(page.locator("[data-document]").filter({ hasText: "main.typ" })).toBeVisible()
    const fileTree = page.locator("#file-tree")
    await expect(fileTree).toBeVisible()
    await expect(fileTree.getByText("MAIN", { exact: true })).toHaveCount(0)

    const preview = page.waitForResponse((response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/preview"))
    await page.locator("#compile-button").click()
    const result = await (await preview).json()
    expect(result.entry).toBe("main.typ")
    await expect(page.locator("#document-path")).toHaveText("chapter.typ")

    const chapterId = await page.locator("[data-document]").filter({ hasText: "chapter.typ" }).getAttribute("data-document")
    expect(chapterId).toBeTruthy()
    const changedPreview = page.waitForResponse((response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/preview"))
    await page.locator("#project-entrypoint").selectOption(chapterId!)
    expect((await (await changedPreview).json()).entry).toBe("chapter.typ")
  })

  test("rapid preview updates do not invalidate an in-flight PDF", async ({ page }) => {
    await signIn(page, { username: "demo", password: "demo", role: "author" })
    await createProject(page, "Rapid Compile Test")
    await openFirstProject(page)

    const failedBlobRequests: string[] = []
    page.on("requestfailed", (request) => {
      if (request.url().startsWith("blob:")) failedBlobRequests.push(request.url())
    })

    for (let index = 0; index < 5; index++) {
      await page.locator("#compile-button").click()
    }

    await expect(page.locator(".pdf-page canvas").first()).toBeVisible({ timeout: 30_000 })
    expect(failedBlobRequests).toEqual([])
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

    await expect(page.locator("#diagnostics-list").getByText(
      "unknown variable: invalid_function_that_does_not_exist"
    )).toBeVisible({ timeout: 30_000 })
  })
})
