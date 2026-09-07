#!/usr/bin/env bash
#
# sync-desktop-apps.sh — ensure every remote desktop has its own app bundle.
#
# For each remote desktop in desktops.toml, ensures
# $HOME/Applications/<app_name>.app exists, where <app_name> is the desktop's
# app_name field if present, else its label. Missing apps are created with
# scripts/create-herdr-app.sh (bundle id com.gustavocaiano.herdr.<id>, with
# every "_" in the id replaced by "-"). Existing apps are never rebuilt or
# overwritten — the daily updater (scripts/update-herdr-app.sh) owns refresh —
# and are only verified with codesign; a bad signature is a warning, never a
# reason to overwrite. This script writes no credentials and never prints
# configuration contents.
#
# This script takes no arguments and accepts no command fragments.

set -euo pipefail

log()  { printf '[sync-desktop-apps] %s\n' "$*"; }
warn() { printf '[sync-desktop-apps] warning: %s\n' "$*" >&2; }
die()  { printf '[sync-desktop-apps] error: %s\n' "$*" >&2; exit 1; }

# Accept no arguments and no command fragments.
[ "$#" -eq 0 ] || die "this script takes no arguments"
[ -n "${HOME:-}" ] || die "HOME is not set"
[ "$(uname)" = "Darwin" ] || die "this script requires macOS"

# Resolve repository root relative to this script, independent of CWD.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

DESKTOPS_TOML="$REPO_DIR/desktops.toml"
CREATE_APP="$REPO_DIR/scripts/create-herdr-app.sh"
APPS_DIR="$HOME/Applications"

[ -f "$DESKTOPS_TOML" ] || die "desktop configuration missing: $DESKTOPS_TOML"
[ -f "$CREATE_APP" ] || die "create-herdr-app.sh missing: $CREATE_APP"

# Parse desktops.toml and emit "<id>\t<app_name>" for remote desktops only,
# where <app_name> is the app_name field if present, else the label. Labels may
# contain spaces (so only the end of the value is trimmed, never the middle)
# but never tabs or control characters (the config parser rejects those).
desktop_apps() {
  awk '
    function reset() { id=""; mode=""; label=""; app_name="" }
    function emit() {
      if (mode == "remote" && id != "" && app_name != "") print id "\t" app_name
    }
    BEGIN { reset() }
    /^\[desktops\./ {
      emit()
      id = $0; sub(/^\[desktops\./, "", id); sub(/\].*$/, "", id)
      mode = ""; label = ""; app_name = ""
      next
    }
    /^[[:space:]]*mode[[:space:]]*=/ {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line)
      gsub(/["\x27]/, "", line); sub(/[[:space:]].*$/, "", line); mode = line
    }
    /^[[:space:]]*label[[:space:]]*=/ {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line)
      gsub(/["\x27]/, "", line); sub(/[[:space:]]+$/, "", line); label = line
      if (app_name == "") app_name = label
    }
    /^[[:space:]]*app_name[[:space:]]*=/ {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line)
      gsub(/["\x27]/, "", line); sub(/[[:space:]]+$/, "", line); app_name = line
    }
    END { emit() }
  ' "$DESKTOPS_TOML"
}

total=0
created=0
present=0

while IFS=$'\t' read -r id app_name; do
  [ -n "$id" ] || continue
  [ -n "$app_name" ] || continue
  total=$((total + 1))
  app="$APPS_DIR/${app_name}.app"
  if [ -d "$app" ]; then
    if codesign --verify --deep --strict "$app" >/dev/null 2>&1; then
      log "exists: $app (signature ok)"
    else
      warn "exists: $app but signature verification failed; recreate it manually with scripts/create-herdr-app.sh if needed (this script never rebuilds existing apps)"
    fi
    present=$((present + 1))
  else
    bundle_id="com.gustavocaiano.herdr.${id//_/-}"
    log "creating $app for remote desktop '${id}' (bundle id ${bundle_id})"
    APP_NAME="$app_name" BUNDLE_ID="$bundle_id" TARGET_APP="$app" "$CREATE_APP"
    log "created $app"
    created=$((created + 1))
  fi
done < <(desktop_apps)

if [ "$total" -eq 0 ]; then
  log "no remote desktops configured; nothing to do"
else
  log "summary: ${total} remote desktop(s): ${created} created, ${present} already present"
fi
