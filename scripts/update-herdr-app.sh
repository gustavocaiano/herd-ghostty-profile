#!/usr/bin/env bash
set -euo pipefail

SOURCE_APP="${SOURCE_APP:-/Applications/Ghostty.app}"
HERDR_APP="${TARGET_APP:-$HOME/Applications/Herdr.app}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="$REPO_DIR/state"
DESKTOPS_TOML="$REPO_DIR/desktops.toml"
LOG_PREFIX='[update-herdr-app]'

log() { printf '%s %s\n' "$LOG_PREFIX" "$*"; }
die() { printf '%s error: %s\n' "$LOG_PREFIX" "$*" >&2; exit 1; }

version_for_app() {
  local app="$1"
  local bin="$app/Contents/MacOS/ghostty"
  [[ -x "$app/Contents/MacOS/ghostty-bin" ]] && bin="$app/Contents/MacOS/ghostty-bin"
  [[ -x "$bin" ]] || return 1
  "$bin" +version | awk '/^- version:/ {print $3; exit}'
}

# Parse desktops.toml and emit "<id>\t<app_name>" for remote desktops only,
# where <app_name> is the app_name field if present, else the label. Same
# derivation as scripts/sync-desktop-apps.sh. Labels may contain spaces (so only
# the end of the value is trimmed, never the middle) but never tabs or control
# characters (the config parser rejects those).
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

# After a successful Ghostty rebuild, if the desktop-switcher is installed (both
# binaries present), verify it still works. Never rebuild/relink it here, and
# never fail the Ghostty update over it; users who never installed it are skipped.
desktop_switcher_post_update_check() {
  local plugin_bin="$REPO_DIR/plugins/herdr-desktop-switcher/bin/herdr-desktop-switcher"
  local launch_bin="$HOME/.local/bin/herdr-desktop-launch"
  if [[ ! -x "$plugin_bin" || ! -x "$launch_bin" ]]; then
    return 0
  fi
  log "desktop-switcher installed; verifying after Ghostty update"
  if ! "$REPO_DIR/scripts/install-desktop-switcher.sh" --check; then
    log "warning: desktop-switcher check reported defects; run scripts/install-desktop-switcher.sh --check for details"
  fi
}

[[ -d "$SOURCE_APP" ]] || die "source app not found: $SOURCE_APP"
mkdir -p "$STATE_DIR"

source_version="$(version_for_app "$SOURCE_APP")" \
  || die "could not determine Ghostty version of $SOURCE_APP"
[[ -n "$source_version" ]] || die "could not determine Ghostty version of $SOURCE_APP"
log "source Ghostty version: $source_version"

# App list: the local Herdr app plus one app per remote desktop. When
# desktops.toml is absent, the local Herdr app is processed alone.
app_names=()
app_paths=()
app_bundle_ids=()
add_app() {
  app_names+=("$1")
  app_paths+=("$2")
  app_bundle_ids+=("$3")
}
add_app "Herdr" "$HERDR_APP" "com.gustavocaiano.herdr"
if [[ -f "$DESKTOPS_TOML" ]]; then
  while IFS=$'\t' read -r id app_name; do
    [[ -n "$id" ]] || continue
    [[ -n "$app_name" ]] || continue
    add_app "$app_name" "$HOME/Applications/${app_name}.app" "com.gustavocaiano.herdr.${id//_/-}"
  done < <(desktop_apps)
fi

up_to_date=0
recreated=0
created=0
skipped_running=0

for i in "${!app_paths[@]}"; do
  app_name="${app_names[$i]}"
  app="${app_paths[$i]}"
  bundle_id="${app_bundle_ids[$i]}"

  app_version="missing"
  if [[ -d "$app" ]]; then
    app_version="$(version_for_app "$app" 2>/dev/null || printf 'unknown')"
  fi

  if [[ "$source_version" == "$app_version" ]]; then
    log "$app is up to date ($app_version)"
    up_to_date=$((up_to_date + 1))
    continue
  fi

  if [[ -d "$app" ]] && pgrep -f "$app/Contents/MacOS/(herdr-launcher|ghostty-bin|ghostty)" >/dev/null 2>&1; then
    log "$app is running; skipping update"
    skipped_running=$((skipped_running + 1))
    continue
  fi

  if [[ "$app_version" == "missing" ]]; then
    log "creating $app (bundle id $bundle_id)"
    created=$((created + 1))
  else
    log "recreating $app ($app_version -> $source_version)"
    recreated=$((recreated + 1))
  fi
  APP_NAME="$app_name" BUNDLE_ID="$bundle_id" TARGET_APP="$app" "$REPO_DIR/scripts/create-herdr-app.sh"
done

printf '%s\n' "$source_version" > "$STATE_DIR/ghostty-version.txt"
log "summary: ${up_to_date} up to date, ${recreated} recreated, ${created} created, ${skipped_running} skipped (running)"
desktop_switcher_post_update_check
