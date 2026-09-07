#!/usr/bin/env bash
#
# sync-configs.sh — install tracked Herdr/Ghostty config snapshots to active paths.
#
# Managed files (the only files this script writes):
#   <repo>/herdr-config.toml   -> ~/.config/herdr/config.toml   (mode 0644)
#   <repo>/ghostty-herdr.conf  -> ~/.config/ghostty/herdr       (mode 0644)
#
# Intentionally untouched: plugins, sessions, logs, sockets, state, and every
# other file under ~/.config/herdr, ~/.config/ghostty, and ~/.local/state.
# This script takes no arguments and accepts no command fragments.

set -euo pipefail

log() { printf '[sync-configs] %s\n' "$*"; }
die() { printf '[sync-configs] error: %s\n' "$*" >&2; exit 1; }

# Accept no arguments and no command fragments.
[ "$#" -eq 0 ] || die "this script takes no arguments"
[ -n "${HOME:-}" ] || die "HOME is not set"

# Resolve repository root relative to this script, independent of CWD.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

SRC_HERDR="$REPO_DIR/herdr-config.toml"
SRC_GHOSTTY="$REPO_DIR/ghostty-herdr.conf"

DST_HERDR_DIR="$HOME/.config/herdr"
DST_GHOSTTY_DIR="$HOME/.config/ghostty"
DST_HERDR="$DST_HERDR_DIR/config.toml"
DST_GHOSTTY="$DST_GHOSTTY_DIR/herdr"

[ -f "$SRC_HERDR" ]   || die "missing tracked source: $SRC_HERDR"
[ -f "$SRC_GHOSTTY" ] || die "missing tracked source: $SRC_GHOSTTY"

# Create only the two managed config directories if absent.
mkdir -p "$DST_HERDR_DIR" "$DST_GHOSTTY_DIR"

# Install only the two managed files with mode 0644.
cp "$SRC_HERDR" "$DST_HERDR"
chmod 0644 "$DST_HERDR"
cp "$SRC_GHOSTTY" "$DST_GHOSTTY"
chmod 0644 "$DST_GHOSTTY"

log "installed $SRC_HERDR -> $DST_HERDR"
log "installed $SRC_GHOSTTY -> $DST_GHOSTTY"

# Reload Herdr config through the supported behavior, when a herdr executable is
# available. If it fails, report it instead of hiding it.
if command -v herdr >/dev/null 2>&1; then
  if ! herdr server reload-config; then
    die "'herdr server reload-config' failed; config files were installed, but Herdr may still use the previous config. Restart Herdr if needed."
  fi
  log "herdr server reload-config completed"
else
  log "'herdr' executable not found; skipped config reload. Restart Herdr to pick up the new config."
fi
