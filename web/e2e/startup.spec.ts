import { expect, test } from "@playwright/test"

test("initializes the public shell without a browser runtime error", async ({ page }) => {
  const runtimeErrors: string[] = []
  page.on("pageerror", (error) => runtimeErrors.push(error.message))

  await page.goto("/")

  await expect(page.getByRole("button", { name: "Sign in" })).toBeVisible()
  expect(runtimeErrors).toEqual([])
})
