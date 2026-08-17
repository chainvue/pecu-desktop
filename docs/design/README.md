# Pecu — Desktop App Design Package

Everything a developer/designer needs to build the Pecu desktop wallet (Windows, macOS, Linux).

## Contents

- `pecu-tokens.json` — all design tokens: colors (dark/light), typography, spacing, radii, component specs, breakpoints, motion values. Single source of truth.
- `screens/Pecu Desktop Screens.html` — the 5 desktop windows (1060×640 reference): Dashboard, Send, Markets + currency detail, Messenger, History + audit log. Open in a browser; keep `support.js` next to it.
- `design-kit/Pecu Design Kit.html` — brand, color system, typography, 20 line icons, component library (dark + light), platform shells, app icon rules, media kit.
- `handoff/Pecu Handoff.html` — states (loading/error/empty/offline/syncing/validation), flow diagrams incl. error paths, responsive rules, platform specifics (Win/macOS/Linux), asset spec, motion spec, full keyboard shortcut map.
- `icons/windows/` — 256/48/32/24/16 PNG (bundle to `.ico`; 24/16 are cursor-less by design).
- `icons/macos/` — 1024→16 PNG (bundle to `.icns` via `iconutil`; deliver borderless, macOS applies the squircle).
- `icons/linux/` — 512/256/128 PNG + use `master.svg` as scalable source for hicolor theme paths.
- `icons/master.svg` — vector master of the app icon (@ + cursor on ink).

## Key rules

- Fonts: JetBrains Mono (wordmark, numbers, amounts, IDs) + Space Grotesk (UI text). Both on Google Fonts.
- Dark: bg #0B0D10, surface #14181D, accent #12D6B4. Light: bg #F4F6F8, accent #0FA38A.
- Min window 960×600; below 960px width switch to the tablet (medium) layout.
- ⌘ = Ctrl on Windows/Linux. Full shortcut map in the handoff file.
- "—" means unknown, not zero. ◈ marks shielded items.
- Icons below 32px drop the cursor; below 24px only the @ remains.
