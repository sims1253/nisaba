import { test, expect } from "@playwright/test"
import { createProject, signIn, USERS } from "./helpers"

async function selectText(page: import("@playwright/test").Page, text: string): Promise<void> {
  await page.locator(".cm-content").click()
  await page.keyboard.press("Control+f")
  const input = page.locator(".cm-search .cm-textfield").first()
  await input.fill(text)
  await input.press("Enter")
  await page.keyboard.press("Escape")
  await expect.poll(() => page.evaluate(() => getSelection()?.toString())).toBe(text)
}

test("stale overlapping reviewer proposal never enters REST before acceptance", async ({ browser }) => {
  const ownerContext = await browser.newContext()
  const reviewerContext = await browser.newContext()
  const owner = await ownerContext.newPage()
  const reviewer = await reviewerContext.newPage()
  await signIn(owner, USERS.author)
  const projectName = await createProject(owner, "Reviewer Overlap Baseline")
  await owner.locator(".cm-content").fill("before alpha-overlap after")
  await expect(owner.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
  const documentId = await owner.locator("[data-document]").first().getAttribute("data-document")
  await owner.locator("#share-button").click()
  await owner.locator("#share-subject").fill("reviewer")
  await owner.locator("#share-role").selectOption("reviewer")
  await owner.locator("#share-invite").click()

  await signIn(reviewer, USERS.reviewer)
  await reviewer.locator("[data-project]", { hasText: projectName }).click()
  await reviewer.locator("[data-document]").first().click()
  await expect(reviewer.locator("#sync-label")).toContainText("Live", { timeout: 10_000 })
  await selectText(reviewer, "alpha-overlap")
  await selectText(owner, "alpha-overlap")
  await owner.keyboard.type("alpha-overlap-AUTHOR")
  await expect(owner.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
  await reviewer.keyboard.type("alpha-overlap-REVIEWER")
  await expect(owner.locator("#review-count")).toHaveText(/[1-9]/, { timeout: 10_000 })
  await pageWait(2_000)

  await owner.locator("#go-projects").click()
  const projectId = await owner.locator("[data-project]", { hasText: projectName }).getAttribute("data-project")
  const body = await owner.evaluate(async ({ projectId, documentId }) => {
    const token = JSON.parse(localStorage.getItem("nisaba.auth.token") ?? "{}").accessToken
    const response = await fetch(`/api/projects/${projectId}/documents/${documentId}`, { headers: { authorization: `Bearer ${token}` } })
    return ((await response.json()) as { body: string }).body
  }, { projectId, documentId })
  expect(body).toContain("alpha-overlap-AUTHOR")
  expect(body).not.toContain("alpha-overlap-REVIEWER")

  await owner.locator("[data-project]", { hasText: projectName }).click()
  await owner.locator("[data-document]").first().click()
  await owner.locator("#review-button").click()
  await owner.getByRole("button", { name: "Accept" }).first().click()
  await expect(owner.locator("#review-count")).toHaveText("0")
  await expect(owner.locator("#save-status")).toHaveText("Saved", { timeout: 10_000 })
  await owner.reload()
  await expect(owner.locator(".cm-content")).toContainText("alpha-overlap-REVIEWER", { timeout: 10_000 })
  await expect(owner.locator("#review-count")).toHaveText("0")

  await ownerContext.close()
  await reviewerContext.close()
})

const pageWait = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))
