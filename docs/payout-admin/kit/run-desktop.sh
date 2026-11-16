#!/usr/bin/env bash
# Launch the built macOS/Linux desktop with a process-only regtest environment.
set -euo pipefail
kit_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="${REPO_ROOT:-$(cd -- "$kit_dir/../../.." && pwd)}"
release_dir="${RELEASE_DIR:-$repo_root/target/release}"
binary="${DESKTOP_BIN:-}"
if [[ -z "$binary" ]]; then
  case "$(uname -s)" in
    Darwin) binary="$release_dir/bundle/macos/Strata Multisig.app/Contents/MacOS/desktop-app" ;;
    Linux)
      shopt -s nullglob
      images=("$release_dir"/bundle/appimage/*.AppImage)
      if [[ ${#images[@]} -eq 1 ]]; then
        binary="${images[0]}"
      elif [[ ${#images[@]} -gt 1 ]]; then
        echo 'Multiple AppImages found. Set DESKTOP_BIN to the one for this build.' >&2; exit 1
      else
        binary="$release_dir/desktop-app"
      fi ;;
    *) echo 'Supported platforms: macOS and Linux.' >&2; exit 1 ;;
  esac
fi
# Both only for this process and the app it starts. ALLOW_INSECURE_HTTP_URL lets the app reach the
# round's coordinator over plain http on the LAN; without it only https or loopback is accepted.
export BITCOIN_NETWORK=regtest
export ALLOW_INSECURE_HTTP_URL=1
printf 'BITCOIN_NETWORK=%s ALLOW_INSECURE_HTTP_URL=%s (launched process only)\nDesktop: %s\n' "$BITCOIN_NETWORK" "$ALLOW_INSECURE_HTTP_URL" "$binary"
if [[ "${1:-}" = --dry-run ]]; then exit 0; fi
[[ -x "$binary" ]] || { echo 'Build the desktop or set DESKTOP_BIN to an executable path.' >&2; exit 1; }
exec "$binary" "$@"
