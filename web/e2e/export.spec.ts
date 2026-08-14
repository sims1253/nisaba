import { test, expect } from "@playwright/test"
import { execFileSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createProject, signIn, USERS } from "./helpers"

test("export includes edits still inside the autosave debounce", async ({ page }) => {
  await signIn(page, USERS.author)
  await createProject(page, "Export Pending Edit")

  const editor = page.locator(".cm-content")
  await editor.click()
  await page.keyboard.type("= Saved baseline")
  await expect(page.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })

  const marker = `pending-export-${Date.now()}`
  await editor.click()
  await page.keyboard.press("Control+End")
  await page.keyboard.type(`\n${marker}`)
  await expect(page.locator("#save-status")).toHaveText("Unsaved changes")

  await page.locator("#export-button").click()
  const responsePromise = page.waitForResponse((response) =>
    response.request().method() === "POST" && response.url().includes("/exports")
  )
  await page.getByRole("button", { name: "Prepare download" }).click()
  const payload = await (await responsePromise).json() as { zip_base64?: string }

  expect(payload.zip_base64).toBeTruthy()
  const directory = mkdtempSync(join(tmpdir(), "nisaba-export-e2e-"))
  try {
    const archive = join(directory, "export.zip")
    writeFileSync(archive, Buffer.from(payload.zip_base64!, "base64"))
    expect(execFileSync("unzip", ["-p", archive], { encoding: "utf8" })).toContain(marker)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})
