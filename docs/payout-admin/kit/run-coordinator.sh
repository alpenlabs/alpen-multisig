#!/usr/bin/env bash
# Run the release coordinator with the adjacent kit environment, in this process only.
set -euo pipefail
kit_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
env_file="${COORDINATOR_ENV:-$kit_dir/coordinator.env}"
[[ "$env_file" = /* ]] || env_file="$PWD/$env_file"
[[ -f "$env_file" ]] || { echo "Missing environment file: $env_file" >&2; exit 1; }
repo_root="${REPO_ROOT:-$(cd -- "$kit_dir/../../.." && pwd)}"
binary="${COORDINATOR_BIN:-$repo_root/target/release/server}"
[[ "$binary" = /* ]] || binary="$PWD/$binary"
cd -- "$(dirname -- "$env_file")"
# main.rs loads .env from CWD/ancestors. Refuse accidental settings from another round.
check_dir="$PWD"
while :; do
  [[ ! -f "$check_dir/.env" ]] || { echo "Move the kit outside the .env tree: $check_dir/.env" >&2; exit 1; }
  [[ "$check_dir" != / ]] || break
  check_dir="$(dirname -- "$check_dir")"
done
set -a
# shellcheck source=coordinator.env
source "$env_file"
set +a
[[ "${BITCOIN_NETWORK:-}" = regtest ]] || { echo 'This kit requires BITCOIN_NETWORK=regtest.' >&2; exit 1; }
printf 'Environment: %s\nCoordinator: %s\nParameters: %s\nNetwork: %s\n' \
  "$env_file" "$binary" "${PAYOUT_ADMIN_PARAMS_PATH:-unset}" "$BITCOIN_NETWORK"
if [[ "${1:-}" = --dry-run ]]; then exit 0; fi
[[ -x "$binary" ]] || { echo 'Build the coordinator or set COORDINATOR_BIN to its absolute path.' >&2; exit 1; }
[[ -f "${PAYOUT_ADMIN_PARAMS_PATH:-}" ]] || { echo 'Seed the round first; params.toml is missing.' >&2; exit 1; }
exec "$binary" "$@"
