/**
 * Shared helpers for Playwright e2e tests.
 * Signs in through the real Keycloak OIDC flow — not a stubbed token.
 */

import { type Page, type BrowserContext, expect } from "@playwright/test"

export const BASE_URL = process.env.E2E_BASE_URL ?? "http://127.0.0.1:8103"
export const KEYCLOAK_URL = process.env.E2E_KEYCLOAK_URL ?? "http://127.0.0.1:8090"

export interface TestUser {
  username: string
  password: string
  role: string
}

export const USERS = {
  author: { username: "demo", password: "demo", role: "author" } satisfies TestUser,
  reviewer: { username: "reviewer", password: "reviewer", role: "reviewer" } satisfies TestUser,
  reader: { username: "reader", password: "reader", role: "read-only" } satisfies TestUser,
} as const

/**
 * Sign in through the full Keycloak OIDC flow.
 * Waits for the redirect back to the app after authentication.
 */
export async function signIn(page: Page, user: TestUser): Promise<void> {
  await page.goto(BASE_URL)
  await page.getByRole("button", { name: "Sign in" }).click()

  // Keycloak login page
  await page.waitForURL(/\/protocol\/openid-connect\/auth/, { timeout: 15_000 })
  await page.locator("#username").fill(user.username)
  await page.locator("#password").fill(user.password)
  await page.getByRole("button", { name: "Sign In" }).click()

  // Wait for redirect back to the app, then for the SPA to process the callback
  await page.waitForURL("http://127.0.0.1:8103/", { timeout: 15_000 }).catch(() => {})
  await page.waitForTimeout(3000)
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible({ timeout: 10_000 })
}

/** Create a project through the UI and return its ID from the URL. */
const uniqueCounter = { value: 0 }

export async function createProject(page: Page, name: string): Promise<void> {
  const uniqueName = `${name}-${Date.now()}-${uniqueCounter.value++}`
  await page.locator("#new-project").waitFor({ state: "visible" })
  await page.locator("#new-project").click()
  await page.waitForSelector("#prompt-input", { state: "visible" })
  await page.locator("#prompt-input").fill(uniqueName)
  await page.locator("#prompt-ok").click()
  // Wait for the document outline to appear (project opened with auto-created main.typ)
  await page.locator("[data-document]").first().waitFor({ state: "visible", timeout: 15_000 })
  await page.locator("[data-document]").first().click()
  await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })
}

/** Click on a project in the sidebar to open it and wait for the editor. */
export async function openFirstProject(page: Page): Promise<void> {
  // If already inside a project view, just wait for the editor
  await page.locator("[data-document]").first().click()
  await page.locator(".cm-content").waitFor({ state: "visible", timeout: 15_000 })
}

/** Create a document through the UI (the ＋ in the navigator's Files header). */
export async function createDocument(page: Page, path: string, _title: string): Promise<void> {
  await page.locator("#add-document").click()
  await page.waitForSelector("#prompt-input", { state: "visible" })
  await page.locator("#prompt-input").fill(path)
  await page.locator("#prompt-ok").click()
  await page.waitForTimeout(1000)
}

/** Get the editor content as text. */
export async function getEditorText(page: Page): Promise<string> {
  return page.locator(".cm-content").inputValue().catch(async () => {
    return page.evaluate(() => {
      const cm = document.querySelector(".cm-content") as HTMLElement
      return cm?.textContent ?? ""
    })
  })
}

/** Type into the editor at the current cursor position. */
export async function typeInEditor(page: Page, text: string): Promise<void> {
  const editor = page.locator(".cm-content")
  await editor.waitFor({ state: "visible", timeout: 15_000 })
  await editor.click()
  await page.keyboard.type(text)
}

/**
 * Create a fresh browser context and sign in as the given user.
 * Returns the page and context for independent session testing.
 */
export async function newSession(
  browser: import("@playwright/test").Browser,
  user: TestUser
): Promise<{ page: Page; context: BrowserContext }> {
  const context = await browser.newContext()
  const page = await context.newPage()
  await signIn(page, user)
  return { page, context }
}
