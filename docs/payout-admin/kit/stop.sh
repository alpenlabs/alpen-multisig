#!/bin/bash
set -eu
cd "$(dirname "$0")"
command -v lsof >/dev/null || { echo 'lsof is missing. Install it first.' >&2; exit 1; }

running() {
    lsof -ta -d txt ./desktop-app ./server ./bitcoind ./electrs 2>/dev/null || true
}

kill $(running) 2>/dev/null || true

for (( attempt = 0; attempt < 100; attempt++ )); do
    [[ -n $(running) ]] || break
    sleep 0.1
done

kill -9 $(running) 2>/dev/null || true
rm -rf /tmp/payout-test
