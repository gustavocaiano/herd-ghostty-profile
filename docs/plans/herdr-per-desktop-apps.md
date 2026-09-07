# Herdr per-desktop app bundles

## Status

Implemented — 2026-09-07.

## Problem

The Desktop Switcher launched every desktop (Local and each remote, e.g.
`devbox`) as a new instance of the same bundle `~/Applications/Herdr.app`
(`com.gustavocaiano.herdr`). Two running instances of one bundle id share a
single Dock/Cmd+Tab identity, so Local and devbox windows were indistinguishable
in the app switcher and both stacked on one Dock icon.

## Design

- Local keeps `~/Applications/Herdr.app` with bundle id `com.gustavocaiano.herdr`.
- Each remote desktop gets a true app identity at
  `$HOME/Applications/<app_name>.app`, named only by the desktop's effective app
  name: the `app_name` field if configured, else its `label` (current `devbox`
  → `Devbox.app`), with `CFBundleName`/`CFBundleDisplayName` to match.
- Remote bundle ids are `com.gustavocaiano.herdr.<suffix>`, where `<suffix>` is
  the desktop id with every `_` replaced by `-` (`devbox` →
  `com.gustavocaiano.herdr.devbox`).
- One switcher-managed running instance per app bundle: the launcher helper
  (`scripts/herdr-desktop-launch.swift`) takes the expected `--bundle-id` per
  desktop and enforces a `com.gustavocaiano.herdr(.*)` family rule for
  launch/activate/terminate; instance registry drift is detected through the
  recorded bundle id and bundle path.
- App bundles are produced by the env-parameterized `scripts/create-herdr-app.sh`
  (`APP_NAME`/`BUNDLE_ID`/`TARGET_APP`), copying with APFS clonefile when
  available and a normal-copy fallback.

## Implementation

- `scripts/sync-desktop-apps.sh` (new): ensures every remote desktop's app
  exists; creates only missing ones and never overwrites existing ones (the
  daily updater owns refresh); verifies existing signatures with `codesign`.
- `scripts/update-herdr-app.sh`: processes `Herdr.app` plus every configured
  remote app independently — skips only apps that are currently running,
  recreates outdated or missing ones — then writes the source Ghostty version
  to `state/ghostty-version.txt` and runs the desktop-switcher read-only
  `--check` once.
- `scripts/install-desktop-switcher.sh`: normal mode ensures missing per-desktop
  apps (via `sync-desktop-apps.sh`) after build/link succeed and before config
  sync; `--check` verifies each remote app's presence, signature, and bundle id.
- The shared `id → app name/path/bundle id` derivation (awk over
  `desktops.toml`) lives in `sync-desktop-apps.sh`, `update-herdr-app.sh`, and
  `install-desktop-switcher.sh`.

## Migration

Existing devbox instance records under `~/.local/state/herdr-desktop-switcher/`
hold the old shared `Herdr.app` bundle id and path. While an old devbox client
window is still open, the first switcher launch after this change refuses with
the "configuration changed while pid N is still open" drift error. Close that
old devbox window once; the stale record is pruned after the close and later
launches use `Devbox.app` normally. `desktops.toml` is unchanged (label
"Devbox" → app "Devbox" via the default rule).

## Explicitly deferred

- Distinct tinted icons per app: all per-desktop apps intentionally reuse
  `assets/herdr.icns` for now.
- `LSMultipleInstancesProhibited`: not set; multiple windows of one desktop app
  remain possible.
- Public standalone repo `gustavocaiano/herdr-desktop-switcher` v0.1 still
  documents and implements the single-bundle design; this personal setup's
  divergence can be reconciled in a future v0.2.
