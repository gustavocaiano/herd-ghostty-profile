#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-Herdr}"
BUNDLE_ID="${BUNDLE_ID:-com.gustavocaiano.herdr}"
SOURCE_APP="${SOURCE_APP:-/Applications/Ghostty.app}"
TARGET_APP="${TARGET_APP:-$HOME/Applications/Herdr.app}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICON_PATH="${ICON_PATH:-$REPO_DIR/assets/herdr.icns}"
GHOSTTY_PROFILE="${GHOSTTY_PROFILE:-$HOME/.config/ghostty/herdr}"
HERDR_BIN="${HERDR_BIN:-$HOME/.local/bin/herdr}"
FORCE="${FORCE:-0}"
CLANG="${CLANG:-/usr/bin/clang}"

log() { printf '[create-herdr-app] %s\n' "$*"; }
die() { printf '[create-herdr-app] error: %s\n' "$*" >&2; exit 1; }

[[ -d "$SOURCE_APP" ]] || die "source app not found: $SOURCE_APP"
[[ -f "$SOURCE_APP/Contents/MacOS/ghostty" ]] || die "source Ghostty executable not found"
[[ -f "$ICON_PATH" ]] || die "icon not found: $ICON_PATH"
[[ -f "$GHOSTTY_PROFILE" ]] || die "Ghostty Herdr profile not found: $GHOSTTY_PROFILE"
[[ -x "$HERDR_BIN" ]] || die "herdr binary not executable: $HERDR_BIN"
[[ -x "$CLANG" ]] || die "clang not found: $CLANG; install Xcode Command Line Tools"

if [[ -d "$TARGET_APP" ]] && pgrep -f "$TARGET_APP/Contents/MacOS/(herdr-launcher|ghostty-bin|ghostty)" >/dev/null 2>&1; then
  if [[ "$FORCE" != "1" ]]; then
    die "$TARGET_APP appears to be running; close it first or run with FORCE=1"
  fi
fi

mkdir -p "$(dirname "$TARGET_APP")"

# Probe once whether cp -c (APFS clonefile) works on the destination filesystem,
# so per-desktop app copies cost ~0 extra disk. The probe is silent and its temp
# files are always removed; plain cp -R remains the fallback.
copy_flags=(-R)
clone_probe_src="$(mktemp "$(dirname "$TARGET_APP")/.herdr-clone-probe.XXXXXX")"
clone_probe_dst="${clone_probe_src}.clone"
printf 'x' > "$clone_probe_src" 2>/dev/null || :
if cp -c "$clone_probe_src" "$clone_probe_dst" 2>/dev/null; then
  copy_flags=(-Rc)
fi
rm -f "$clone_probe_src" "$clone_probe_dst"

rm -rf "$TARGET_APP"
log "copying $SOURCE_APP -> $TARGET_APP"
cp "${copy_flags[@]}" "$SOURCE_APP" "$TARGET_APP"

PLIST="$TARGET_APP/Contents/Info.plist"
RESOURCES="$TARGET_APP/Contents/Resources"
MACOS="$TARGET_APP/Contents/MacOS"
ORIGINAL_BIN="$MACOS/ghostty"
RENAMED_BIN="$MACOS/ghostty-bin"
LAUNCHER="$MACOS/herdr-launcher"

log "patching Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $APP_NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $APP_NAME" "$PLIST" 2>/dev/null || /usr/libexec/PlistBuddy -c "Add :CFBundleDisplayName string $APP_NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleIconFile $APP_NAME" "$PLIST" 2>/dev/null || /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string $APP_NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Delete :CFBundleIconName" "$PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable herdr-launcher" "$PLIST"

log "installing icon"
cp "$ICON_PATH" "$RESOURCES/$APP_NAME.icns"

log "installing native launcher"
mv "$ORIGINAL_BIN" "$RENAMED_BIN"
launcher_src="$(mktemp /tmp/herdr-launcher.XXXXXX.c)"
cat > "$launcher_src" <<'EOF'
#include <errno.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static char *join2(const char *a, const char *b) {
  size_t len = strlen(a) + strlen(b) + 1;
  char *out = (char *)calloc(len, 1);
  if (!out) return NULL;
  snprintf(out, len, "%s%s", a, b);
  return out;
}

static char *arg_with_value(const char *key, const char *value) {
  size_t len = strlen(key) + strlen(value) + 1;
  char *out = (char *)calloc(len, 1);
  if (!out) return NULL;
  snprintf(out, len, "%s%s", key, value);
  return out;
}

int main(int argc, char **argv) {
  // If Herdr.app is launched from inside an existing Herdr pane, LaunchServices
  // may preserve HERDR_* environment variables. Clear them so the new app
  // attaches normally instead of being rejected as a nested Herdr instance.
  unsetenv("HERDR_ENV");
  unsetenv("HERDR_SOCKET_PATH");
  unsetenv("HERDR_SESSION");
  unsetenv("HERDR_ACTIVE_WORKSPACE_ID");
  unsetenv("HERDR_ACTIVE_TAB_ID");
  unsetenv("HERDR_ACTIVE_PANE_ID");
  unsetenv("HERDR_ACTIVE_PANE_CWD");

  char exe_path[PATH_MAX];
  uint32_t exe_path_size = sizeof(exe_path);
  if (_NSGetExecutablePath(exe_path, &exe_path_size) != 0) {
    fprintf(stderr, "herdr-launcher: executable path is too long\n");
    return 127;
  }

  char *last_slash = strrchr(exe_path, '/');
  if (!last_slash) {
    fprintf(stderr, "herdr-launcher: cannot determine executable directory\n");
    return 127;
  }
  *last_slash = '\0';

  char ghostty_bin[PATH_MAX];
  snprintf(ghostty_bin, sizeof(ghostty_bin), "%s/ghostty-bin", exe_path);

  const char *profile = getenv("HERDR_GHOSTTY_PROFILE");
  const char *herdr = getenv("HERDR_BIN_PATH");
  const char *home = getenv("HOME");

  char *default_profile = NULL;
  char *default_herdr = NULL;
  if (!profile || profile[0] == '\0') {
    if (!home || home[0] == '\0') {
      fprintf(stderr, "herdr-launcher: HOME is not set and HERDR_GHOSTTY_PROFILE was not provided\n");
      return 127;
    }
    default_profile = join2(home, "/.config/ghostty/herdr");
    profile = default_profile;
  }
  if (!herdr || herdr[0] == '\0') {
    if (!home || home[0] == '\0') {
      fprintf(stderr, "herdr-launcher: HOME is not set and HERDR_BIN_PATH was not provided\n");
      return 127;
    }
    default_herdr = join2(home, "/.local/bin/herdr");
    herdr = default_herdr;
  }

  char *config_arg = arg_with_value("--config-file=", profile);
  char *command_arg = arg_with_value("--command=", herdr);
  if (!config_arg || !command_arg) {
    fprintf(stderr, "herdr-launcher: out of memory\n");
    return 127;
  }

  int extra = argc > 1 ? argc - 1 : 0;
  char **child_argv = (char **)calloc((size_t)extra + 4, sizeof(char *));
  if (!child_argv) {
    fprintf(stderr, "herdr-launcher: out of memory\n");
    return 127;
  }

  child_argv[0] = ghostty_bin;
  child_argv[1] = config_arg;
  child_argv[2] = command_arg;
  for (int i = 0; i < extra; i++) child_argv[3 + i] = argv[1 + i];
  child_argv[3 + extra] = NULL;

  execv(ghostty_bin, child_argv);
  fprintf(stderr, "herdr-launcher: execv failed for %s: %s\n", ghostty_bin, strerror(errno));
  return 127;
}
EOF
if ! "$CLANG" -Os -arch arm64 -arch x86_64 "$launcher_src" -o "$LAUNCHER" 2>/dev/null; then
  "$CLANG" -Os "$launcher_src" -o "$LAUNCHER"
fi
rm -f "$launcher_src"
chmod +x "$LAUNCHER"

log "ad-hoc signing"
codesign --force --deep --sign - "$TARGET_APP" >/dev/null

log "validating signature"
codesign --verify --deep --strict "$TARGET_APP"

log "done: $TARGET_APP"
