#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export CARGO_TARGET_DIR="$PWD/target"
source_dir="$PWD/docs/payout-admin"
target="$source_dir/target"
kit="$target/payout-admin-test-kit"
version=30.5

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) platform=arm64-apple-darwin; checksum=4c69d3519af2dafe94b695a8c128dd7756a8ded73799fae609085f2e4aeb5a9e ;;
    Linux-x86_64) platform=x86_64-linux-gnu; checksum=593de13690ea3ef15e7ac3a695cc60ca9c2d576a184aeec69c69f458c7707568 ;;
    Linux-aarch64) platform=aarch64-linux-gnu; checksum=161b769e91c1d18c87de3d9e059b45bace1c56d2e95c29ff04260b424b2e85ae ;;
    *) echo "Unsupported system: $(uname -sm)" >&2; exit 1 ;;
esac

archive="bitcoin-$version-$platform.tar.gz"
rm -rf "$kit"
mkdir -p "$kit"

if [[ ! -f "$target/$archive" ]]; then
    curl -fL "https://bitcoincore.org/bin/bitcoin-core-$version/$archive" -o "$target/$archive.part"
    mv "$target/$archive.part" "$target/$archive"
fi

(cd "$target" && echo "$checksum  $archive" | shasum -a 256 -c -) || { rm "$target/$archive"; exit 1; }
tar -xf "$target/$archive" -C "$kit" --strip-components=2 "bitcoin-$version/bin/bitcoind" "bitcoin-$version/bin/bitcoin-cli"
# electrs bundles a RocksDB that misses this include for GCC 13 and newer (Debian 13+).
CXXFLAGS="${CXXFLAGS:-} -include cstdint" cargo install electrs --version 0.10.7 --locked --root "$target/electrs"
cp "$target/electrs/bin/electrs" "$kit/"

cargo build --release -p orchestrator-be --bin server
cargo build --release -p alpen-multisig-e2e-tests --features regtest-kit --bin regtest-bridge
cargo build --release -p alpen-multisig-e2e-tests --bin signer
npm ci
VITE_ORCHESTRATOR_BASE_URL=http://127.0.0.1:18503/api/v1 npm run build --workspace desktop-app
TAURI_CONFIG='{"identifier":"com.strata.multisig.payout-test"}' cargo build --release -p desktop-app --bin desktop-app --features tauri/custom-protocol
cp target/release/{server,desktop-app,regtest-bridge} "$kit/"
cp target/release/signer "$kit/admin2"
ln -s admin2 "$kit/admin3"
cp "$source_dir"/kit/{.env,bitcoin.conf,electrs.toml,start.sh,stop.sh,pbcopy,pbpaste} "$source_dir"/{build,guide}.md "$kit/"
