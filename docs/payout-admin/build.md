# Building the Payout Admin Tester Kit

The kit is a folder that contains the following items:
1. The multisig app, built from production code for local testing
2. The coordinator server
3. Two admin cosigners (`admin2`, `admin3`)
4. A dummy bridge that pushes transactions to the blockchain (`regtest-bridge`)
5. Bitcoin Core (`bitcoind`, `bitcoin-cli`)
6. An Electrum server (`electrs`)
7. Helper scripts (`start.sh`, `stop.sh`, `pbcopy`, `pbpaste`)
8. Documentation (`build.md`, `guide.md`)

The kit runs on macOS and Debian.

## Prerequisites

1. Install the prerequisites from [Building from Source](../../README.md#building-from-source) in the README.
2. Install the tools for your system.

macOS:

```sh
xcode-select --install
brew install cmake
```

Debian:

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev librsvg2-dev \
  libayatana-appindicator3-dev libudev-dev libusb-1.0-0-dev libssl-dev \
  clang libclang-dev cmake curl libdigest-sha-perl lsof xclip wl-clipboard ledger-wallets-udev
```

## Build

```sh
cd docs/payout-admin
./package.sh
```

The kit lands in `docs/payout-admin/target/payout-admin-test-kit/`.
Now go to [guide.md](guide.md).
