<!-- Default files for a shared Payout Administrator regtest round. -->
# Regtest kit

Follow [the setup guide](../setup.html) in order. Copy this kit to a fresh sibling directory outside the repository for
each round. Run scripts, do not source them. Keep generated chain data and `seed-keys.json` out of Git.

`bitcoin.conf` is the host's node: regtest, transaction index, RPC on loopback only, and peer-to-peer on port 18444 for
the testers' nodes. Its RPC credentials are `user` / `password`, the values the desktop app's Local mode uses, so a host
who is also a tester can use it directly. They are acceptable only because RPC stays on loopback and the chain is
regtest.

`tester-bitcoin.conf` is each tester's own node. Replace `HOST` in `connect=HOST:18444` with the host's LAN address. It
follows the host's chain and serves the desktop app on the same machine through loopback RPC with the Local-mode
credentials.

`coordinator.env` is trusted Bash configuration sourced by the coordinator launcher. It lists every variable read by
`orchestrator-be/src/config.rs`, plus `RUST_LOG`. It enables payouts using `./seed/params.toml`, relative to this file,
and unsets `DATABASE_URL` for in-memory storage. A restart loses coordinator records and sessions. For deliberate
Postgres use, replace the unset locally with a connection string; never commit it. `STRATA_ADMIN_STATE_RPC_URL` is
explicit even though the code supplies a default; no ASM service is needed for this payout round.

`run-coordinator.sh` loads the adjacent env file only in the child process, checks regtest and seeded params, and
executes `target/release/server`. Use `REPO_ROOT=/absolute/repository` when running a copied kit, or
`COORDINATOR_BIN=/absolute/binary`. `COORDINATOR_ENV` selects another trusted env file. It refuses `.env` in the env
directory or its ancestors because the binary's dotenv loader could otherwise reintroduce settings such as
`DATABASE_URL`. `--dry-run` prints paths without requiring the build or seed output.

`run-desktop.sh` sets `BITCOIN_NETWORK=regtest` and `ALLOW_INSECURE_HTTP_URL=1` for the launched process only. The
second lets the app reach the round's coordinator over plain http on the LAN. On macOS it starts the built
`.app/Contents/MacOS/desktop-app`; on Linux it selects one AppImage, falling back to the unbundled release binary.
Multiple AppImages require an explicit choice. It never writes a file or shell profile. `REPO_ROOT`, `RELEASE_DIR`, or
`DESKTOP_BIN` override discovery. `--dry-run` prints the selection without launching.

`roster.example.toml` is a deliberately invalid placeholder template. Copy it to `roster-input.toml`, replace every
fingerprint and xpub with `ledger_identity` output, and include exactly one entry per tester in the agreed order. Use the
seeder's validated `roster.toml` in the app, alongside the corresponding `params.toml`.

## Why Local mode

On this branch the app's Orchestrator URL field exists only in the "Local node" connection mode, and the Rust side
refuses plain http to another machine unless `ALLOW_INSECURE_HTTP_URL` is set. So every tester runs their own regtest
node peered with the host, uses Local mode with the coordinator's LAN URL, and starts the app through `run-desktop.sh`.
