# macOS e2e (experimental)

The acceptance suite for `platform: "macos"`: Tapsmith testing a SwiftUI app on the Mac it runs on.
It runs against `test-app-macos/` with `tapsmith.config.macos.mjs`.

## Running it

From the repository root, build the test app once:

```bash
./test-app-macos/build.sh
```

Then from `e2e/`:

```bash
npx tapsmith test -c tapsmith.config.macos.mjs
```

The first run builds the macOS agent and caches it in `~/.tapsmith/macos-agent/`.

## Before you run

- **Leave the Mac alone while it runs.** The tests drive this Mac's screen, keyboard and clipboard. If
  another app keeps focus, the test app can't come to the front and the run fails to start.
  Keep the Mac unlocked and awake.
- **Automation Mode.** On a Mac that has never run UI tests, XCUITest asks to enable Automation
  Mode. To allow it without a password prompt (CI machines need this), run once:
  `sudo automationmodetool enable-automationmode-without-authentication`

## What it covers, and what it doesn't

The suite taps, types, toggles, checks that each test starts from the app's initial state, and
checks that an absent element times out cleanly.

macOS is sequential `tapsmith test` only. Resets relaunch the app; they can't clear app data
yet, so a test app that persisted data would carry it between tests. Network capture, permissions,
swipe gestures, UI mode, watch mode, parallel workers and the MCP server aren't supported yet.
