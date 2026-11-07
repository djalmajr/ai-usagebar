# App Icon, favorites and provider headers: visual review

The original favorites/picker comparisons use upstream `59573ff` before and UI `4e13965` after. The header/focus follow-up compares `4e13965` before with `6afb529` after.

- `favorites-contact.png`: Classic, Native macOS and Native Windows, light/dark.
- `picker-contact.png`: macOS Menu Bar Shows picker in Classic and Native, light/dark.
- `headers-contact.png`: single-account provider header before/after, all six style/theme combinations.
- `account-focus-comparison.png`: multiple accounts, selected by keyboard, before/after in all six combinations. The Native Windows focus indicator was previously invisible.

Each comparison labels before and after. Captures use production Vite bundles and the repository's fictitious fixture data in isolated Chromium. Windows is a browser preview, not native Windows execution.

`native-gauge-on-white.png` is an actual AppKit NSImage render at 18 points and 2x, on white for legibility. It is not a capture of the installed menu bar; native menu-bar capture was unavailable.
