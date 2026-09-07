#!/bin/bash
#
# install-desktop-switcher.sh — install the Herdr Desktop Switcher plugin.
#
# Normal mode (no arguments):
#   - requires macOS and an existing ~/Applications/Herdr.app (never rebuilds it,
#     even when it is running);
#   - requires absolute tools (cargo, swiftc, codesign, herdr, ssh);
#   - builds the Rust plugin with `cargo build --release --locked`;
#   - installs the Rust binary and the Swift launcher as fresh temp inodes in the
#     same directory as their final path, chmod 0755, ad-hoc codesigns and verifies
#     them, then atomically renames them into place. A running installed inode is
#     never overwritten or re-signed — the old inode stays alive for any running
#     process while new invocations get the new binary;
#   - ensures the tracked command/summon shims are executable;
#   - links+enables the plugin via `$HOME/.local/bin/herdr plugin link <path> --enabled`;
#   - ensures every remote desktop has its own app bundle (e.g.
#     ~/Applications/Devbox.app) by running scripts/sync-desktop-apps.sh, which
#     creates missing apps and never rebuilds existing ones, only after
#     build/link succeed;
#   - applies tracked Herdr/Ghostty configs with scripts/sync-configs.sh only after
#     build/link succeed;
#   - prints the bounded-SSH requirement for each remote target as an actionable
#     warning. It never writes credentials or SSH config.
#
# --check: read-only verification of the local installation. Performs no builds or
#   writes. Alongside the Herdr.app, binary, shim, plugin-link, and config checks,
#   it verifies every remote desktop's per-desktop app bundle: present, signature
#   valid, and bundle id matching com.gustavocaiano.herdr.<id>. Exits nonzero for
#   required local installation defects; unbounded or offline remotes are warnings
#   so Local remains usable.
#
# This script accepts only an optional `--check` and no other arguments.

set -euo pipefail

LOG_PREFIX='[install-desktop-switcher]'
log()  { printf '%s %s\n' "$LOG_PREFIX" "$*"; }
warn() { printf '%s warning: %s\n' "$LOG_PREFIX" "$*" >&2; }
die()  { printf '%s error: %s\n' "$LOG_PREFIX" "$*" >&2; exit 1; }

# ---- argument policy: only an optional --check ----
MODE="install"
if [ "$#" -gt 1 ]; then
  die "this script accepts only an optional --check argument"
fi
if [ "$#" -eq 1 ]; then
  case "$1" in
    --check) MODE="check" ;;
    *) die "unknown argument: $1; only --check is accepted" ;;
  esac
fi

# ---- resolve repository and fixed paths relative to this script ----
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PLUGIN_DIR="$REPO_DIR/plugins/herdr-desktop-switcher"
PLUGIN_BIN_DIR="$PLUGIN_DIR/bin"
PLUGIN_BIN="$PLUGIN_BIN_DIR/herdr-desktop-switcher"
SWIFT_SRC="$REPO_DIR/scripts/herdr-desktop-launch.swift"
COMMAND_SHIM="$REPO_DIR/scripts/herdr-desktop-command"
SUMMON_SHIM="$PLUGIN_DIR/scripts/summon.sh"
DESKTOPS_TOML="$REPO_DIR/desktops.toml"
HERDR_BIN="$HOME/.local/bin/herdr"
LOCAL_BIN_DIR="$HOME/.local/bin"
LAUNCH_BIN="$LOCAL_BIN_DIR/herdr-desktop-launch"
HERDR_APP="$HOME/Applications/Herdr.app"
SYNC_CONFIGS="$REPO_DIR/scripts/sync-configs.sh"
SYNC_DESKTOP_APPS="$REPO_DIR/scripts/sync-desktop-apps.sh"

# Temp files created during install; cleaned up on exit so a failed install never
# leaves a half-written binary at the final path.
_TMP_FILES=()
cleanup() {
  [ "${#_TMP_FILES[@]}" -gt 0 ] || return 0
  local f
  for f in "${_TMP_FILES[@]}"; do
    rm -f "$f"
  done
}
trap cleanup EXIT

require_macos() {
  [ "$(uname)" = "Darwin" ] || die "this installer requires macOS"
}

# require_absolute_tool <var> <tool> [explicit-path]
# Resolves an absolute, executable path. Prefers an explicit path when it is
# executable, otherwise looks up <tool> on PATH. Fails the script if unresolved.
require_absolute_tool() {
  local var="$1" tool="$2" explicit="${3:-}" path
  if [ -n "$explicit" ] && [ -x "$explicit" ]; then
    path="$explicit"
  else
    path="$(command -v "$tool" 2>/dev/null || true)"
  fi
  [ -n "$path" ] || die "$tool not found; ensure it is installed and on PATH"
  case "$path" in
    /*) ;;
    *) die "$tool resolved to a non-absolute path: $path" ;;
  esac
  [ -x "$path" ] || die "$tool is not executable: $path"
  printf -v "$var" '%s' "$path"
}

# Resolve tools needed for a real install (cargo + swiftc + signing + herdr).
resolve_install_tools() {
  require_absolute_tool CARGO cargo
  require_absolute_tool CODESIGN codesign
  require_absolute_tool SSH ssh /usr/bin/ssh
  # Swift compiler: prefer the active developer toolchain, then PATH.
  local swift_path
  swift_path="$(xcrun --find swiftc 2>/dev/null || true)"
  if [ -z "$swift_path" ] || [ ! -x "$swift_path" ]; then
    swift_path="$(command -v swiftc 2>/dev/null || true)"
  fi
  [ -n "$swift_path" ] || die "swiftc not found; install Xcode Command Line Tools"
  case "$swift_path" in
    /*) ;;
    *) die "swiftc resolved to a non-absolute path: $swift_path" ;;
  esac
  [ -x "$swift_path" ] || die "swiftc is not executable: $swift_path"
  SWIFTC="$swift_path"
  # The direct CLT swiftc needs an explicit SDKROOT to locate the stdlib; the
  # xcrun shim sets it automatically. Setting it explicitly is safe for both.
  SDKROOT="$(xcrun --show-sdk-path 2>/dev/null || true)"
  [ -n "$SDKROOT" ] || die "could not determine SDK path; install Xcode Command Line Tools"
  # herdr is required at the fixed user path used by the manifest and runtime.
  [ -x "$HERDR_BIN" ] || die "herdr binary not executable: $HERDR_BIN"
}

# Resolve tools needed for --check (signing + ssh only; no cargo/swiftc builds).
resolve_check_tools() {
  require_absolute_tool CODESIGN codesign
  require_absolute_tool SSH ssh /usr/bin/ssh
}

# Fresh-inode install with ad-hoc codesigning and atomic rename.
# install_fresh_inode <source> <final>
#   <source> non-empty: copy that file into a fresh temp inode in the same
#   directory as <final>, sign, verify, then atomic rename over <final>.
#   <source> empty: compile SWIFT_SRC into the fresh temp inode instead.
install_fresh_inode() {
  local source="$1" final="$2" dir tmp
  dir="$(dirname "$final")"
  [ -d "$dir" ] || die "target directory does not exist: $dir"
  tmp="$(mktemp "$dir/.$(basename "$final").XXXXXX")" \
    || die "could not create temp file in $dir"
  _TMP_FILES+=("$tmp")
  if [ -n "$source" ]; then
    cp "$source" "$tmp"
  else
    SDKROOT="$SDKROOT" "$SWIFTC" -O "$SWIFT_SRC" -o "$tmp"
  fi
  chmod 0755 "$tmp"
  "$CODESIGN" --force --sign - "$tmp" >/dev/null
  "$CODESIGN" --verify --strict "$tmp" >/dev/null
  # Atomic same-directory rename. If <final> already exists and is running, the
  # old inode stays alive for the running process; new invocations get this one.
  mv -f "$tmp" "$final"
  log "installed $final"
}

# Parse desktops.toml and emit "<id>\t<target>" for remote desktops only.
remote_targets() {
  awk '
    function reset() { id=""; mode=""; target="" }
    BEGIN { reset() }
    /^\[desktops\./ {
      if (mode == "remote" && target != "") print id "\t" target
      id = $0; sub(/^\[desktops\./, "", id); sub(/\].*$/, "", id)
      mode = ""; target = ""
      next
    }
    /^[[:space:]]*mode[[:space:]]*=/ {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line)
      gsub(/["\x27]/, "", line); sub(/[[:space:]].*$/, "", line); mode = line
    }
    /^[[:space:]]*target[[:space:]]*=/ {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line)
      gsub(/["\x27]/, "", line); sub(/[[:space:]].*$/, "", line); target = line
    }
    END { if (mode == "remote" && target != "") print id "\t" target }
  ' "$DESKTOPS_TOML"
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

# Derive the bundle id for a remote desktop app: com.gustavocaiano.herdr.<id>
# with every "_" in the desktop id replaced by "-".
remote_bundle_id() {
  printf 'com.gustavocaiano.herdr.%s\n' "${1//_/-}"
}

# Print the bounded-SSH requirement for each remote target (actionable, no secret).
print_ssh_requirements() {
  [ -f "$DESKTOPS_TOML" ] || return 0
  local id target
  while IFS=$'\t' read -r id target; do
    [ -n "$target" ] || continue
    warn "remote desktop '${id}' uses SSH target '${target}'. Before remote QA, configure ~/.ssh/config with bounded non-interactive options (BatchMode yes, StrictHostKeyChecking yes, ConnectTimeout 5, ConnectionAttempts 1, ServerAliveInterval 3, ServerAliveCountMax 1) and non-interactive key authentication. Run '$0 --check' to verify. No credentials are stored by this installer."
  done < <(remote_targets)
}

# ---- check mode helpers (all read-only) ----

# ssh_effective_is_bounded <target>: 0 bounded, 1 unbounded, 2 inspect failed.
ssh_effective_is_bounded() {
  local target="$1" cfg val ok=1
  cfg="$("$SSH" -G "$target" 2>/dev/null)" || return 2
  val="$(printf '%s\n' "$cfg" | awk '/^batchmode /{print $2; exit}')"
  [ "$val" = "yes" ] || ok=0
  val="$(printf '%s\n' "$cfg" | awk '/^stricthostkeychecking /{print $2; exit}')"
  { [ "$val" = "yes" ] || [ "$val" = "true" ]; } || ok=0
  val="$(printf '%s\n' "$cfg" | awk '/^connecttimeout /{print $2; exit}')"
  { [[ "$val" =~ ^[0-9]+$ ]] && (( val > 0 && val <= 10 )); } || ok=0
  val="$(printf '%s\n' "$cfg" | awk '/^connectionattempts /{print $2; exit}')"
  [ "$val" = "1" ] || ok=0
  val="$(printf '%s\n' "$cfg" | awk '/^serveraliveinterval /{print $2; exit}')"
  { [[ "$val" =~ ^[0-9]+$ ]] && (( val > 0 && val <= 10 )); } || ok=0
  val="$(printf '%s\n' "$cfg" | awk '/^serveralivecountmax /{print $2; exit}')"
  { [[ "$val" =~ ^[0-9]+$ ]] && (( val > 0 && val <= 3 )); } || ok=0
  [ "$ok" = "1" ] || return 1
  return 0
}

# ssh_live_reachable <target>: 0 reachable, nonzero offline/unreachable.
# Bounded by SSH's own timeouts; BatchMode=yes prevents any prompt.
ssh_live_reachable() {
  local target="$1"
  "$SSH" -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=3 \
         -o ConnectionAttempts=1 -o ServerAliveInterval=3 -o ServerAliveCountMax=1 \
         "$target" true >/dev/null 2>&1
}

# verify_adhoc_signature <path>: 0 ok, nonzero bad. Does not print contents.
verify_adhoc_signature() {
  local path="$1" details
  "$CODESIGN" --verify --strict "$path" >/dev/null 2>&1 || return 1
  details="$("$CODESIGN" -dv "$path" 2>&1)" || return 1
  case "$details" in
    *"Signature=adhoc"*) ;;
    *) return 1 ;;
  esac
  return 0
}

run_check() {
  local required_failures=0
  log "check mode: read-only verification (no builds or writes)"

  # 1. App exists + signature.
  if [ ! -d "$HERDR_APP" ]; then
    warn "required: $HERDR_APP not found; run scripts/create-herdr-app.sh first"
    required_failures=$((required_failures + 1))
  elif ! "$CODESIGN" --verify --deep --strict "$HERDR_APP" >/dev/null 2>&1; then
    warn "required: $HERDR_APP signature verification failed; recreate with scripts/create-herdr-app.sh"
    required_failures=$((required_failures + 1))
  else
    log "ok: $HERDR_APP present and signed"
  fi

  # 1b. Per-desktop app bundles: one per remote desktop, signed, expected id.
  # Read-only: --check never creates, modifies, or re-signs apps.
  if [ -f "$DESKTOPS_TOML" ]; then
    local id app_name app expected_id actual_id app_ok
    while IFS=$'\t' read -r id app_name; do
      [ -n "$id" ] || continue
      [ -n "$app_name" ] || continue
      app="$HOME/Applications/${app_name}.app"
      expected_id="$(remote_bundle_id "$id")"
      if [ ! -d "$app" ]; then
        warn "required: $app not found; run scripts/sync-desktop-apps.sh to create it"
        required_failures=$((required_failures + 1))
        continue
      fi
      app_ok=0
      if "$CODESIGN" --verify --deep --strict "$app" >/dev/null 2>&1; then
        app_ok=1
      else
        warn "required: $app signature verification failed; recreate it with scripts/create-herdr-app.sh (APP_NAME='${app_name}' BUNDLE_ID='${expected_id}' TARGET_APP='$app')"
        required_failures=$((required_failures + 1))
      fi
      actual_id="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$app/Contents/Info.plist" 2>/dev/null || true)"
      if [ "$actual_id" != "$expected_id" ]; then
        warn "required: $app has bundle id '${actual_id:-<none>}' but desktop '${id}' expects '${expected_id}'; recreate it with scripts/create-herdr-app.sh (APP_NAME='${app_name}' BUNDLE_ID='${expected_id}' TARGET_APP='$app')"
        required_failures=$((required_failures + 1))
        app_ok=0
      fi
      if [ "$app_ok" -eq 1 ]; then
        log "ok: $app present, signed, bundle id ${expected_id}"
      fi
    done < <(desktop_apps)
  fi

  # 2. Installed Rust binary executable + ad-hoc signature.
  if [ ! -x "$PLUGIN_BIN" ]; then
    warn "required: plugin binary not executable: $PLUGIN_BIN; run $0 to install"
    required_failures=$((required_failures + 1))
  elif ! verify_adhoc_signature "$PLUGIN_BIN"; then
    warn "required: plugin binary signature invalid: $PLUGIN_BIN; run $0 to reinstall"
    required_failures=$((required_failures + 1))
  else
    log "ok: plugin binary $PLUGIN_BIN executable and ad-hoc signed"
  fi

  # 2b. Installed Swift launcher executable + ad-hoc signature.
  if [ ! -x "$LAUNCH_BIN" ]; then
    warn "required: launcher not executable: $LAUNCH_BIN; run $0 to install"
    required_failures=$((required_failures + 1))
  elif ! verify_adhoc_signature "$LAUNCH_BIN"; then
    warn "required: launcher signature invalid: $LAUNCH_BIN; run $0 to reinstall"
    required_failures=$((required_failures + 1))
  else
    log "ok: launcher $LAUNCH_BIN executable and ad-hoc signed"
  fi

  # 3. Command shim + summon syntax.
  if [ ! -f "$COMMAND_SHIM" ]; then
    warn "required: command shim missing: $COMMAND_SHIM"
    required_failures=$((required_failures + 1))
  elif ! /bin/bash -n "$COMMAND_SHIM"; then
    warn "required: command shim syntax error: $COMMAND_SHIM"
    required_failures=$((required_failures + 1))
  else
    log "ok: command shim syntax"
  fi
  if [ ! -f "$SUMMON_SHIM" ]; then
    warn "required: summon script missing: $SUMMON_SHIM"
    required_failures=$((required_failures + 1))
  elif ! /bin/bash -n "$SUMMON_SHIM"; then
    warn "required: summon script syntax error: $SUMMON_SHIM"
    required_failures=$((required_failures + 1))
  else
    log "ok: summon script syntax"
  fi

  # 4. Plugin present + enabled in `herdr plugin list --json`.
  if [ ! -x "$HERDR_BIN" ]; then
    warn "required: herdr binary not executable: $HERDR_BIN"
    required_failures=$((required_failures + 1))
  else
    local plugin_json
    plugin_json="$("$HERDR_BIN" plugin list --plugin herdr-desktop-switcher --json 2>/dev/null || true)"
    if printf '%s' "$plugin_json" | grep -Eq '"plugin_id"[[:space:]]*:[[:space:]]*"herdr-desktop-switcher"'; then
      if printf '%s' "$plugin_json" | grep -Eq '"enabled"[[:space:]]*:[[:space:]]*true'; then
        log "ok: plugin herdr-desktop-switcher is linked and enabled"
      else
        warn "required: plugin herdr-desktop-switcher is linked but not enabled; run: $HERDR_BIN plugin enable herdr-desktop-switcher"
        required_failures=$((required_failures + 1))
      fi
    else
      warn "required: plugin herdr-desktop-switcher is not linked; run $0 to install"
      required_failures=$((required_failures + 1))
    fi
  fi

  # 5. Tracked keybinding/config paths active (semantic/text check; no contents printed).
  local active_herdr_cfg="$HOME/.config/herdr/config.toml"
  local active_ghostty="$HOME/.config/ghostty/herdr"
  if [ ! -f "$active_herdr_cfg" ]; then
    warn "required: active Herdr config not found: $active_herdr_cfg; run $0 (or scripts/sync-configs.sh)"
    required_failures=$((required_failures + 1))
  elif grep -q 'cmd+shift+k' "$active_herdr_cfg" \
      && grep -q 'herdr-desktop-switcher.open' "$active_herdr_cfg"; then
    log "ok: Cmd+Shift+K -> herdr-desktop-switcher.open binding is active"
  else
    warn "required: Cmd+Shift+K desktop-switcher binding not active in $active_herdr_cfg; run $0 (or scripts/sync-configs.sh)"
    required_failures=$((required_failures + 1))
  fi
  if [ ! -f "$active_ghostty" ]; then
    warn "required: active Ghostty Herdr profile not found: $active_ghostty; run $0 (or scripts/sync-configs.sh)"
    required_failures=$((required_failures + 1))
  else
    log "ok: Ghostty Herdr profile is active"
  fi

  # 6. Effective SSH safety per remote (warnings only, never fatal).
  if [ -f "$DESKTOPS_TOML" ]; then
    local id target bounded
    while IFS=$'\t' read -r id target; do
      [ -n "$target" ] || continue
      bounded=0
      ssh_effective_is_bounded "$target" || bounded=$?
      case "$bounded" in
        0)
          if ssh_live_reachable "$target"; then
            log "ok: remote '${id}' target '${target}' SSH config is bounded and host is reachable"
          else
            warn "remote '${id}' target '${target}' is bounded but currently offline/unreachable (Local remains usable)"
          fi
          ;;
        1)
          warn "remote '${id}' target '${target}' has unbounded SSH config: require BatchMode yes, StrictHostKeyChecking yes, ConnectTimeout<=10, ConnectionAttempts 1, ServerAliveInterval<=10, ServerAliveCountMax<=3 in ~/.ssh/config (Local remains usable)"
          ;;
        *)
          warn "remote '${id}' target '${target}': could not inspect effective SSH config (Local remains usable)"
          ;;
      esac
    done < <(remote_targets)
  fi

  if [ "$required_failures" -gt 0 ]; then
    warn "check found $required_failures required local installation defect(s)"
    return 1
  fi
  log "check passed"
  return 0
}

# ---- normal install ----
run_install() {
  require_macos
  resolve_install_tools

  [ -d "$HERDR_APP" ] \
    || die "Herdr.app not found at $HERDR_APP; run scripts/create-herdr-app.sh first (this installer never rebuilds it)"
  [ -f "$PLUGIN_DIR/Cargo.toml" ] || die "plugin manifest missing: $PLUGIN_DIR/Cargo.toml"
  [ -f "$SWIFT_SRC" ] || die "Swift source missing: $SWIFT_SRC"
  [ -f "$DESKTOPS_TOML" ] || die "desktop configuration missing: $DESKTOPS_TOML"
  [ -f "$SYNC_CONFIGS" ] || die "sync-configs.sh missing: $SYNC_CONFIGS"
  [ -f "$SYNC_DESKTOP_APPS" ] || die "sync-desktop-apps.sh missing: $SYNC_DESKTOP_APPS"
  [ -x "$SYNC_DESKTOP_APPS" ] || die "sync-desktop-apps.sh not executable: $SYNC_DESKTOP_APPS"

  log "building Rust plugin (cargo build --release --locked)"
  "$CARGO" build --release --locked --manifest-path "$PLUGIN_DIR/Cargo.toml"

  local release_bin="$PLUGIN_DIR/target/release/herdr-desktop-switcher"
  [ -x "$release_bin" ] || die "cargo build did not produce $release_bin"

  # Install the Rust binary as a fresh inode in the plugin bin/ directory.
  mkdir -p "$PLUGIN_BIN_DIR"
  install_fresh_inode "$release_bin" "$PLUGIN_BIN"

  # Compile + install the Swift launcher as a fresh inode under ~/.local/bin.
  mkdir -p "$LOCAL_BIN_DIR"
  install_fresh_inode "" "$LAUNCH_BIN"

  # Ensure the tracked command/summon shims are executable.
  chmod 0755 "$COMMAND_SHIM" "$SUMMON_SHIM"

  # Link + enable the plugin via the fixed herdr path.
  log "linking plugin herdr-desktop-switcher"
  "$HERDR_BIN" plugin link "$PLUGIN_DIR" --enabled

  # Ensure every remote desktop has its own app bundle (creates missing ones;
  # existing ones are only verified, never rebuilt), after build/link succeed.
  log "ensuring per-desktop app bundles"
  "$SYNC_DESKTOP_APPS"

  # Apply tracked configs only after build/link succeed.
  log "applying tracked configs"
  "$SYNC_CONFIGS"

  log "install complete"
  print_ssh_requirements
}

require_macos
if [ "$MODE" = "check" ]; then
  resolve_check_tools
  run_check
else
  run_install
fi
