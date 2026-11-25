#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"

if [[ $(uname -s) == Darwin ]]; then
    xattr -dr com.apple.quarantine .
fi

./stop.sh

for port in 18500 18501 18503 18504; do

    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        echo "Port $port is in use. Stop the program that uses it, then run ./start.sh again." >&2
        exit 1
    fi

done

state=/tmp/payout-test
mkdir -p "$state/home" "$state/bitcoin"
./bitcoind -conf="$PWD/bitcoin.conf" </dev/null >"$state/bitcoin.log" 2>&1 &
./bitcoin-cli -conf="$PWD/bitcoin.conf" -rpcwait -rpcwaittimeout=60 getblockchaininfo >/dev/null ||
    { echo "Bitcoin Core did not start. See $state/bitcoin.log." >&2; exit 1; }
# Give electrs a current tip before setup.
./bitcoin-cli -conf="$PWD/bitcoin.conf" generatetodescriptor 1 'raw(51)' >/dev/null
./electrs --skip-default-conf-files --conf "$PWD/electrs.toml" </dev/null >"$state/electrs.log" 2>&1 &

for (( attempt = 0; attempt < 60; attempt++ )); do

    if (exec 3<>/dev/tcp/127.0.0.1/18501 && echo '{"id":1,"method":"blockchain.headers.subscribe","params":[]}' >&3 &&
        IFS= read -r -t 1 reply <&3 && [[ $reply == *'"height"'* ]]) 2>/dev/null; then
        break
    fi

    sleep 1
done

(( attempt < 60 )) || { echo "electrs did not answer on port 18501. See $state/electrs.log." >&2; exit 1; }
export XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}"
HOME="$state/home" CFFIXED_USER_HOME="$state/home" ./desktop-app </dev/null >"$state/app.log" 2>&1 &
