# Herdr Desktop Switcher

The Desktop Switcher is a Herdr plugin that opens a popup picker (`Cmd+Shift+K`)
and launches or focuses one endpoint-specific Herdr client window per desktop
(Local, `devbox`, and any future SSH-accessible Herdr server). Each desktop runs
under its own true macOS app identity — Local under `Herdr.app`, `devbox` under
`Devbox.app` — so the Dock and Cmd+Tab distinguish desktops. It does not mirror
workspaces and does not retarget an already-open window.

## Architecture

```text
Herdr.app    (com.gustavocaiano.herdr)          ── Local Herdr client window
Devbox.app   (com.gustavocaiano.herdr.devbox)   ── Devbox Herdr client window
Remote N.app (com.gustavocaiano.herdr.<id>)     ── Remote N Herdr client window
        ▲ launched/focused by the Desktop Switcher plugin popup
        └── tracked desktops.toml (data, no credentials)
```

Per-desktop app rules:

- A remote desktop's app is `$HOME/Applications/<app_name>.app`, named only by
  the desktop's effective app name: the `app_name` field if configured, else
  its `label` (current `devbox` → `~/Applications/Devbox.app`).
- Local keeps `Herdr.app` with bundle id `com.gustavocaiano.herdr`; each remote
  app gets `com.gustavocaiano.herdr.<suffix>` with every `_` in the desktop id
  replaced by `-` (`devbox` → `com.gustavocaiano.herdr.devbox`).
- `Cmd+Shift+K` launches or focuses that desktop's app, so Herdr and Devbox are
  distinct in the Dock and Cmd+Tab. All per-desktop apps intentionally reuse the
  same Herdr icon for now.

Tracked artifacts (no credentials, no SSH config, no runtime state in Git):

| Path | Role |
|---|---|
| `plugins/herdr-desktop-switcher/` | Rust plugin. The release binary is installed to `plugins/herdr-desktop-switcher/bin/herdr-desktop-switcher` (`bin/` is gitignored). |
| `plugins/herdr-desktop-switcher/herdr-plugin.toml` | Plugin manifest: the `picker` pane runs `./bin/herdr-desktop-switcher picker`; the `open` action runs `scripts/summon.sh`. |
| `scripts/herdr-desktop-launch.swift` | macOS launcher helper. Compiled to `~/.local/bin/herdr-desktop-launch` (ad-hoc signed). |
| `scripts/herdr-desktop-command` | Generic command shim used as `HERDR_BIN_PATH`; validates the desktop and execs the switcher `client` subcommand. |
| `desktops.toml` | Declarative endpoint list. Remote `target` values are SSH aliases resolved through `~/.ssh/config` (never credentials). |
| `herdr-config.toml` | Herdr config snapshot, including the `cmd+shift+k` binding. Synced to `~/.config/herdr/config.toml`. |
| `ghostty-herdr.conf` | Ghostty profile snapshot. Synced to `~/.config/ghostty/herdr`. |
| `scripts/install-desktop-switcher.sh` | Installer + read-only `--check`. |
| `scripts/sync-desktop-apps.sh` | Ensures every remote desktop's app bundle exists; creates missing ones via `scripts/create-herdr-app.sh` (APFS clonefile when available, normal-copy fallback), never overwrites existing ones. |
| `scripts/sync-configs.sh` | Installs the two tracked config snapshots to their active paths. |

Runtime state (not in Git): `~/.local/state/herdr-desktop-switcher/` (mode 0700)
holds instance records (`<id>.toml`, mode 0600) and pending markers
(`<id>.pending`, mode 0600).

## Install

```bash
~/.config/herd/scripts/install-desktop-switcher.sh
```

The installer requires macOS and an existing `~/Applications/Herdr.app` (build it
first with `scripts/create-herdr-app.sh` if needed). It never rebuilds the app.
It runs `cargo build --release --locked`, installs the Rust binary and the Swift
launcher as fresh inodes (chmod 0755, ad-hoc codesign, verify, atomic rename),
ensures the command/summon shims are executable, runs
`herdr plugin link <plugin-dir> --enabled`, then ensures every remote desktop
has its own app bundle by running `scripts/sync-desktop-apps.sh` (missing apps
are created; existing ones are never rebuilt), and finally applies tracked
configs with `scripts/sync-configs.sh`. A running installed binary is never
overwritten or re-signed: the new binary is written to a fresh temp inode in the
same directory and atomically renamed into place, so any running process keeps
its old inode.

The installer does not write credentials or SSH config. It prints an actionable
warning for each remote target describing the bounded-SSH options that
`~/.ssh/config` must provide before remote QA.

## Check

```bash
~/.config/herd/scripts/install-desktop-switcher.sh --check
```

`--check` performs no builds or writes. It verifies: `Herdr.app` and every
per-desktop app bundle exist and are signed, and each per-desktop bundle id
matches `com.gustavocaiano.herdr.<id>`; the installed Rust and Swift binaries
are executable and ad-hoc signed; the command shim and summon script have valid
bash syntax; the plugin is linked
and enabled (`herdr plugin list --json`); and the active config has the
`Cmd+Shift+K -> herdr-desktop-switcher.open` binding plus the active Ghostty
profile (a semantic/text check that does not print config contents). It then
reports effective SSH safety for each remote as `pass` or a clear warning.

`--check` exits nonzero only for required local installation defects. Unbounded
or offline remotes are warnings, so Local remains usable.

## Usage

Press `Cmd+Shift+K` in Herdr to open the picker. Select a desktop to launch or
focus its endpoint-specific client window: Local opens/focuses `Herdr.app`,
`devbox` opens/focuses `Devbox.app`, each under its own Dock/Cmd+Tab identity.
The default `desktops.toml` provides `local` and `devbox`.

## Status truth

A `Launched`/`Focused` result means the desktop's app instance was started or
activated. It does **not** mean the remote endpoint is reachable or that the
remote Herdr server is ready. The picker reports running/offline/unknown status
from the instance registry and a bounded SSH preflight; a successful launch only
confirms the app process came up with the right identity, not that the remote
session is live.

## SSH alias requirements

Remote desktops resolve `target` through SSH aliases in `~/.ssh/config`. Each
remote target must be a bounded, non-interactive alias:

```sshconfig
Host dev
    HostName your-dev-host
    User you
    BatchMode yes
    StrictHostKeyChecking yes
    ConnectTimeout 5
    ConnectionAttempts 1
    ServerAliveInterval 3
    ServerAliveCountMax 1
    IdentityFile ~/.ssh/id_ed25519
```

Requirements: `BatchMode yes`, `StrictHostKeyChecking yes`, `ConnectTimeout` ≤ 10
(`5` recommended), `ConnectionAttempts 1`, `ServerAliveInterval` ≤ 10 (`3`
recommended), `ServerAliveCountMax` ≤ 3 (`1` recommended), and non-interactive
key authentication. No credentials are stored in this repository.

The current default config has `devbox` with `target = "dev"`. The `dev` SSH
alias **must be configured before remote QA**. Run
`install-desktop-switcher.sh --check` to verify effective SSH safety per remote.

## Instance and pending state; safe recovery

Each launch writes an instance record to
`~/.local/state/herdr-desktop-switcher/<id>.toml` (pid, launch date, bundle id,
mode, target, session, keybindings). Before launching, a `<id>.pending` marker
is created and cleared only after the record is written. Stale registry entries
are reaped by process liveness.

If a launch has an unknown outcome (helper timeout or exit 75), the pending
marker intentionally blocks retries to avoid duplicate clients. Safe recovery:

1. Run `install-desktop-switcher.sh --check`.
2. Inspect `~/.local/state/herdr-desktop-switcher/<id>.pending` and `<id>.toml`.
3. Confirm no live process matches the recorded pid (`ps -p <pid>`).
4. Remove the stale `<id>.pending` marker (and the `<id>.toml` record if stale)
   to unblock the next launch.

If a desktop's configuration changed while a client is still open, the launcher
refuses to start a conflicting endpoint; close the old client first.

## App and update behavior

Every desktop runs under its own app bundle (see the rules above); app copies
use APFS clonefile when available, with a normal-copy fallback. Ghostty
refreshes are handled by `scripts/update-herdr-app.sh`, which processes
`Herdr.app` plus every configured remote app independently: apps already at the
source Ghostty version are left alone, apps that are currently running are
skipped (logged, never fatal), and outdated or missing apps are recreated with
`scripts/create-herdr-app.sh`, preserving the running-app guard and ad-hoc
signing flow. After the loop, if both desktop-switcher binaries are installed,
the updater runs `install-desktop-switcher.sh --check` (read-only) once and
warns on defects without failing the update. It never rebuilds or relinks the
switcher, and users who never installed it are unaffected.

## Migration from the single-bundle design

Earlier versions launched every desktop as a new instance of the same
`Herdr.app`. Existing devbox instance records under
`~/.local/state/herdr-desktop-switcher/` still hold that shared bundle id and
path. While an old devbox client window is still open, the first switcher
launch after this change refuses with the "configuration changed while pid N is
still open" drift error. Close that old window once; the stale record is pruned
after the close and later launches use `Devbox.app` normally.

## Mirror coexistence and migration

`herdr-mirror` (separate plugin) can coexist: Mirror mirrors a remote server's
workspaces into the local sidebar, while Desktop Switcher launches direct
endpoint-specific clients that never mix workspaces. To migrate away from
Mirror:

1. Disable Mirror: `herdr plugin disable mirror` (do not delete it initially).
2. Confirm remote workspaces and panes remain intact on each remote server.
3. After confirmation, remove local mirror representations and mirror-specific
   bindings/state.
4. Keep rollback instructions below to re-enable Mirror if needed.

Do not delete remote sessions or panes during migration.

## Rollback

To disable the Desktop Switcher:

1. Disable the plugin: `herdr plugin disable herdr-desktop-switcher`.
2. Remove the `cmd+shift+k` binding from the active config (or re-sync without
   it via `scripts/sync-configs.sh` after editing the tracked snapshot).
3. Leave the normal Local launch path unchanged.
4. Use `herdr --remote <target> --session <session> --remote-keybindings local`
   manually for direct remote access.
5. Re-enable `herdr-mirror` if a unified sidebar is temporarily preferred.
6. Do not delete remote sessions or panes during rollback.

## Troubleshooting

- **Picker does not open / plugin not linked:** run
  `install-desktop-switcher.sh --check`; reinstall with
  `install-desktop-switcher.sh` if it reports defects.
- **Remote launch fails with unbounded SSH config:** configure the target alias
  in `~/.ssh/config` per the requirements above, then re-run `--check`.
- **"unresolved previous launch without a verifiable registry record":** a
  pending marker blocks retries. Inspect
  `~/.local/state/herdr-desktop-switcher/<id>.pending`, confirm no live process
  matches, and remove the stale marker.
- **"configuration changed while pid N is still open":** close the old client
  before launching the new endpoint.
- **A per-desktop app is missing (e.g. `Devbox.app`):** run
  `scripts/sync-desktop-apps.sh`; it creates only missing apps and never
  overwrites existing ones. The installer runs the same step.
- **First `devbox` launch after the per-desktop change refuses with
  "configuration changed while pid N is still open":** an old devbox client
  window from the single-bundle design is still open and its registry record
  holds the shared `Herdr.app` identity. Close that old window once; the record
  is pruned and the next launch uses `Devbox.app`.
- **After a Ghostty update the picker is broken:** run
  `scripts/update-herdr-app.sh` (it checks the switcher), or run
  `install-desktop-switcher.sh --check`.
- **macOS says a binary is not verified:** the Rust and Swift binaries are
  ad-hoc signed; allow them in Privacy & Security if macOS blocks them.
