#!/bin/bash
set -euo pipefail

: "${HERDR_PLUGIN_ROOT:?HERDR_PLUGIN_ROOT is required}"
exec "${HERDR_PLUGIN_ROOT}/bin/herdr-desktop-switcher" summon
