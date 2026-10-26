import { describe, expect, test } from "tapsmith"

// The macOS slice's acceptance suite, against test-app-macos.

describe("macOS test app", () => {
  // Relaunch before every test, so each starts from the app's initial state.
  test.use({ appResetScope: "test" })

  test("taps, types and toggles", async ({ device }) => {
    await device.getByTestId("incrementButton").tap()
    await expect(device.getByTestId("countLabel")).toHaveText("Count: 1")

    await device.getByTestId("nameField").clearAndType("Ada")
    await expect(device.getByTestId("greeting")).toHaveText("Hello, Ada")

    const agree = device.getByRole("checkbox", { name: "I agree" })
    await expect(agree).not.toBeChecked()
    await agree.tap()
    await expect(agree).toBeChecked()
    await expect(device.getByTestId("agreeState")).toHaveText("Agreed")
  })

  test("starts each test from the app's initial state", async ({ device }) => {
    // The previous test counted to 1: the relaunch between tests resets it.
    await expect(device.getByTestId("countLabel")).toHaveText("Count: 0")
    await expect(device.getByTestId("greeting")).toHaveText("Hello, nobody")
  })

  test("an absent element is not visible", async ({ device }) => {
    // The not-found path must time out cleanly, never fail as an internal error.
    await expect(device.getByTestId("doesNotExist")).not.toBeVisible()
  })
})
