# Spec: Admin Block Payouts (Payout Administrators)

> Status: implemented on branch `feat/payout-admin` (local), 2026-11-11; updated 2026-11-13.
> Requirements: the Payout Admin design doc (Payout Administrator multisig) and
> [`07-supplementary-false-claim-reports.md`](../0-prd/07-supplementary-false-claim-reports.md).
> Bridge: strata-bridge `main` at `dc926c42` (PR #948 merged).

## What it does

A false claim by a bridge operator is stopped by spending the claim's **admin payout connector** (claim output 1)
through its single tap leaf, `multi_a(threshold, admin keys…)`. The Payout Administrators do this together:

1. An admin pastes **false claim reports**. Each one is verified against the admin's own Bitcoin Core node. Invalid
   reports, and claims whose connector is already spent in a block, are ignored, with the reason shown.
2. The app bundles the valid claims into as few transactions as the TRUC size limit allows. It counts the fee inputs
   and one change output. The admin sets the fee rate (0.1–10,000 sat/vB, in 0.1 steps) and sees the total fee. The
   admin can remove inputs, and the shape of each transaction stays fixed. "Insufficient funds" and "size limit" are
   critical errors that block Confirm.
3. **Confirm** signs each transaction on the admin's Ledger. The claim inputs are signed with the admin's params.toml
   key; the fee inputs, from the Admin Wallet, are signed by key path. Each transaction is stored on the coordinator
   in **Pending**.
4. Other admins **Sign** on their Ledger. They can also exchange signatures out of band: *Copy all signatures* /
   *Paste signatures*, or *Export* / *Import transaction*. Only valid signatures are imported; invalid ones are
   listed in the PRD warning text. When the quorum is met the app broadcasts at once. A failed broadcast keeps the
   transaction in Pending, shows the error, and offers **Retry broadcast**.
5. A broadcast transaction moves to **Past**: Unconfirmed or Confirmed (with the block time). While it is
   unconfirmed it can be rebroadcast or copied with its signatures.

A pending transaction expires **4 days after its first signature**, or when one of its inputs is spent in a block by
another transaction. It is then removed. Inputs shared by several pending transactions are marked.

The screen works with **no ASM**: it reads only Bitcoin Core and the coordinator.

## False claim reports

A report (JSON, camelCase, `version` 1) names the claim to block and proves that the same operator's earlier game
ended in a watchtower ACK:

```json
{ "type": "strata-false-claim-report", "version": 1, "network": "regtest", "createdAt": "…",
  "operatorPubkey": "<x-only hex>",
  "newClaimTxid": "…", "newContestTxid": "…",
  "ackedClaimTxid": "…", "ackedContestTxid": "…", "counterproofAckTxid": "…" }
```

`type`, `version`, `network` and `createdAt` are optional and must match when present. There is no support for earlier
versions: a report of another version, or with other fields, is refused (`oldClaimTxid` and the like belong to
an earlier draft). The counterproof and the watchtower index are not part of a report.

Each report is checked against **mined transactions the app fetches by txid** from the admin's Bitcoin Core node
(`-txindex`; no index of spenders is needed, as the report names the transactions):

1. All five transactions are mined, and the ACK has at least 6 confirmations. A report with a younger ACK is rejected
   with a message that says how many confirmations it has; the admin pastes it again later. Reorganisations deeper
   than 6 blocks are not handled.
2. Each contest spends its claim's contest connector (claim output 0) through a contest leaf
   `<N/N> OP_CHECKSIGVERIFY <watchtower> OP_CHECKSIG`, the control block commits the leaf to that output, and the N/N
   key is one of the covenants derived from `params.toml`.
3. The ack is the watchtower ACK of the acked contest, told from the other two spenders of the contest's payout
   connector as the bridge's `is_ack_of` does: it spends the payout connector (output 1) but not the proof connector
   (output 0); it is a version 3 transaction with locktime 0, two inputs and one taproot output; and its counterproof
   input reveals the timeout leaf `<N/N> OP_CHECKSIGVERIFY <delta> OP_CSV` with the N/N key of the acked contest's
   covenant. For a mined transaction consensus has already validated that spend, so that key proves the covenant signed
   the ack. The counterproof itself is not fetched.
4. Both claims pay their CPFP connector (output 3) to `operatorPubkey`.

The new claim's output 1 must also be the admin payout connector of the N/N key its contest revealed. Cost on the node:
a `getrawtransaction` and a `getblockheader` per distinct transaction (shared transactions are fetched once), a
`gettxout` per new claim and one call for the tip.

## Components

| Where | What |
|-------|------|
| `payout-admin-core/` | No-I/O library shared by the app and the coordinator: deployment (`params.toml` + roster), report verification (4 checks), PSBT build / normalize / merge / finalize, signature line format, exact size model, planner, Ledger policies. The connector is built with the bridge's own `strata-bridge-connectors`. |
| `orchestrator-be` `/payouts/*` | Coordination only. Stores pending/past payouts apart from ASM proposals: Postgres tables `payout_pending` / `payout_past`, or in memory. Updates use optimistic concurrency. It authenticates Payout Administrators against `params.toml` (`PAYOUT_ADMIN_PARAMS_PATH`). It re-normalizes and merges every PSBT it receives, sweeps expired entries, and checks broadcast and spent inputs with its own node. |
| `desktop-app/src-tauri` `application/payouts.rs` | The flows: identify the Ledger, sign in, overview, verify reports, plan, confirm, sign, paste, import, export, broadcast, retry, rebroadcast. Device access goes through the `PayoutDevice` trait (`LedgerPayoutDevice` in the app, a software signer in tests). |
| `desktop-app/src-tauri` `infrastructure/payout_*` | Bitcoin Core access (`getrawtransaction`, `gettxout`, `scantxoutset`, `sendrawtransaction`), the coordinator HTTP client, and the registered-policy HMAC store (`payout-policies.json` in app data). |
| `desktop-app/src/domain/block-payouts` | UI: sign-in panel (with the sign-in checklist), Pending/Past tabs, the create flow, the sign/paste/import modals, and the Admin Wallet dialog. Pending/Past are polled every 15 s. *Back to multisigs* returns to the multisig selection with the Ledger still connected; the view warns when Bitcoin Core is older than 28 (no TRUC relay). |

## Trust model

- **`params.toml` is the root.** It is the bridge's own file, parsed with the bridge's parser. It gives the admin
  keys, the threshold and the operator schedule, and from these every covenant's N/N key and connector.
- **Signer roster** (separate file, [example](../operations/examples/payout-signers.example.toml)). It adds each
  admin's Ledger account xpub and master fingerprint, which `params.toml` does not hold. Each entry is checked
  against `params.toml`: `xpub/0/0` must equal the admin key at the same position, or the roster is rejected.
- **Identity.** The connected Ledger's fingerprint and account xpub must match one roster entry. Sign-in is a BIP-137
  signature with the params.toml key (`m/86'/coin'/73'/0/0`). The app checks the recovered key before sending, and
  the coordinator checks membership against its own `params.toml`.
- **Same deployment.** After sign-in, the app compares the coordinator's deployment fingerprint with its own and
  refuses to work with a coordinator that uses a different `params.toml`.
- **Nothing from the coordinator is signed as is.** Every PSBT is normalized first. Normalizing rebuilds the scripts,
  key origins and witness UTXOs from the deployment and re-verifies every signature; a transaction over 10,000 vB or
  with a scriptSig on an input is refused.
- **A pending transaction is checked before the device is asked** (`check_pending_for_signing`, on review and on
  sign). It must be the transaction asked for (its id); every input must exist on the admin's own node with the
  stated amount and script and be unspent, so the fee the device shows is the real one; the fee rate must be within
  the fee field's range; and the single output must be an Admin Wallet address (first 1,000 change or receive
  addresses) of one of the Payout Administrators. The checklist names whose.
- **Confirm signs what was reviewed.** The checklist carries the unsigned ids of the transactions it describes, and
  Confirm signs only if it builds exactly those again (an id commits to every coin, the change address and the
  amounts). It also re-checks on the node that each claim input is the covenant's connector and unspent.
- **What the screen shows about a pending transaction is read from its PSBT**, with verified signatures: inputs,
  signers, quorum, fee, size, and what a paste added. A record that is not a payout of this deployment is not shown.
  Rebroadcast sends the recorded transaction only if it is the payout of that record.
- **The coordinator does not take an admin's word for a broadcast.** On a success report it submits the transaction
  to its own node before moving the payout to Past. The app counts a transaction as broadcast once the admin's node
  accepted it; the next refresh reconciles the coordinator.
- **Device check.** When a covenant policy is registered for the first time, the app requires the Ledger's address
  0 for that policy to be the connector address.

## Transactions

- Version 3 (TRUC), locktime 0, every input `nSequence = 0xffffffff`, at most 10,000 vB. Fee coins must be confirmed:
  a TRUC transaction cannot spend an unconfirmed non-TRUC parent.
- Inputs: claim connectors first (330 sat each), then BIP-86 fee inputs. One P2TR change output, to the first Admin
  Wallet change address that no current coin, pending payout or past payout uses.
- `SIGHASH_DEFAULT` everywhere, so the txid does not change as signatures are added.
- The exchange format is a PSBT (base64). A signature line is `txid:input:xonly:sig64`.

## Ledger

- Claim inputs: one policy per covenant, `tr(@0/**,multi_a(k,@1/**,…,@n/**))`. `@0` is the BIP-328 synthetic xpub
  of the N/N key; `@i` is `[fp/86'/coin'/73']xpub` in params.toml order. Its first receive address is the
  connector, as checked in `payout-admin-core` tests and again on the device at registration. The device asks once
  to register the policy. The HMAC is kept in app data.
- Fee inputs: the default `tr(@0/**)` policy on the same account (the Admin Wallet).
- **Bitcoin app 2.4.6 or later.** At connect the app reads the open app (`GET_VERSION`): it must be the network's
  app (*Bitcoin* on mainnet, *Bitcoin Test* otherwise) at 2.4.6 or later, or the admin is asked to open or update it.
  2.4.6 is the app on the Nano X the design targets (OS 2.7.0, the newest app for that OS); 2.5.1 needs OS 2.7.1.
- **The app shows what the device will show, for its app version.** Before every device interaction the screen lists
  the device's screens in order (`payout_sign_in_review`, `payout_review_sign`, `payout_review_confirm`), built from the
  very message or PSBTs that are then signed, with the device's labels; the app adds only who owns each key and where
  the change goes. The admin approves on the device only when every screen matches, and rejects otherwise. Amounts read
  like the device (`0.0000033 TEST`, `BTC` on mainnet) with the satoshis in parentheses. Registration appears only for
  a payout wallet this device has not registered yet. For an app newer than the checked versions the list says so.
- What the device shows (captured on Speculos, Nano X, both versions; the visual guide has every screen):

  | Step | 2.4.x | 2.5+ |
  |---|---|---|
  | Sign-in | *Review message*, `Path 86'/coin'/73'/0/0`, the message over several pages (it names `Role: payout_admin` and the challenge), *Sign message* | same |
  | Registration | *Review account to register*, `Account name Payout <id>`, `Wallet policy tr(@0/**,multi_a(…))`, *Review co-signer public keys*: `Their key@0` (the bridge key), `Our key@i` / `Their key@j` (`[fp/86'/coin'/73']tpub…`, paged), *Register account* | adds `Primary spending path: Main path: spendable by @0` and `Spending path #1: Any k of @1, @2 and @3 must sign` before the policy |
  | Claim pass | *Security risk: There are external inputs* → *Continue anyway*, *Review transaction*, `From Payout <id>`, `Amount` and `To` (the change, which the multisig sees as another wallet), `Fees`, *Sign transaction* | `From account`, `Amount`, `To`, `External amounts` (the fee inputs), `You spend` (the claim inputs), `Fees` |
  | Fee pass | warning → *Continue anyway*, `Amount 0 TEST (self-transfer)`, `Fees` | warning, `External amounts` (the claim inputs), `You spend` or `You receive` (fee inputs minus change), `Fees` |

  The external-inputs warning is expected: each pass signs only its own inputs. `Fees` is the fee of the whole
  transaction. On the emulated 2.4.6 the claim pass showed `Amount`, `To` and `Fees` without values; the physical
  Nano X (2.4.6) shows them, so that is an artefact of the emulator image. A blank value on a device means reject.
- **Several covenants.** The device signs one payout wallet per pass. A payout with claims of several covenants is,
  per transaction: for each covenant its registration (if needed) and then its claim pass, where the other
  covenants' claims count as external like the fee inputs, then the fee pass. The checklist lists exactly that
  (checked on Speculos, 2.4.6 and 2.5.1, two covenants).
- **Same app as at sign-in.** The app version is read again before a checklist is shown and before signing; another
  version asks for a new sign-in, because the screens differ.
- **The device may lock on many claims.** It loads and signs for about 2 seconds per claim without asking for
  anything (measured on a Nano X, 2.4.6: loading 4 s + 2.0 s per claim, signing 1.9 s per claim, loading the fee
  pass 3 s + 1.3 s per claim), and its PIN lock counts that as inactivity. A lock while it works ends the signing.
  From 15 claims the checklist warns and gives the two remedies: press a button now and then, or lengthen the PIN
  lock.
- **Show an address on the device.** *Admin Wallet → Show on Ledger* displays the Admin Wallet receive address
  (default `tr(@0/**)` policy) and the app checks it equals its own.
- Supported: Ledger Nano X and other Ledgers with the Bitcoin app 2.4.6 or later. Trezor and BitBox are not supported,
  as the design requires (and neither could sign a pay-to-anchor output; see D6).

## Configuration

| Where | Setting |
|-------|---------|
| Coordinator | `PAYOUT_ADMIN_PARAMS_PATH` (enables the feature), `BITCOIN_NETWORK` (must match), `BITCOIN_RPC_*` (needs `-txindex`), `DATABASE_URL` (durable store). See the runbook. |
| Desktop, Node settings | *Bridge parameters file* (`params.toml`), *Signer roster*, and Bitcoin RPC. The node needs `-txindex` to look up report transactions. Fee coins are found with `scantxoutset`, which scans the whole UTXO set: slow on mainnet. Results are cached for 60 s for fee previews, and Confirm always rescans. |

## Coordinator rules

- A pending payout expires 4 days after its first signature (an unsigned import: 4 days after it was stored), or when
  one of its inputs is spent in a block by another transaction. **Expiry is final for that transaction id**
  (`payout_expired`): the same PSBT cannot be submitted again to restart the clock. `GET /payouts/expired` lists
  those ids, and the app creates the payout anew with the next change address, which is a new transaction.
- A payout whose own transaction is found in the mempool or a block is recorded as Past, also when it was completed
  outside the coordinator. This needs `-txindex` on the coordinator's node once the transaction is mined; without
  it the payout is kept as pending and the screen shows a warning.
- Submit, expiry and the move to Past are safe against each other (optimistic versions in memory, an advisory lock
  per transaction id in Postgres).

## Decisions

Decided by Alpen (2026-11-13): D1, D5, D6, D7.

- **D1.** A mempool spend of an input does not expire a pending transaction; only a confirmed spend does. Mempool
  conflicts are replaceable, and a pending payout must not vanish because of an unconfirmed transaction.
- **D2.** Duplicate inputs across pending transactions are allowed and marked, as the design asks. The first one to
  confirm wins; the coordinator then expires the others.
- **D3.** A paste that is not JSON at all is refused as a whole. A JSON item that is not a valid report is ignored
  and counted.
- **D4.** The "first unused change address" is judged from what Bitcoin Core and the coordinator show: current coins,
  and pending and past payouts. The screen does not need the Electrum-synced Admin Wallet. An address that held a coin once
  is remembered in the app data folder (`payout-used-addresses.json`) and is not handed out again after the coin is
  spent. A coin that was received and spent while the app never looked is not known.
- **D5. No Admin ID tools for Payout Administrators.** The payout keys live in `params.toml` (and the roster); the app
  does not show the payout Admin ID, certificate or show-on-device. Deviation from PRD 06 §3.c.i and §4.a.
- **D6. No fee bumping of payouts for now.** RBF is rejected (every bump would need the quorum again). CPFP from the
  creator's change or an anchor output were considered; a keyless anchor (P2A) would display only on Ledger (as a
  generic address; Trezor and BitBox02 refuse it). Deviation from PRD 06 §4.b.iii for payout transactions; a payout
  whose fee went stale is rebuilt.
- **D7. Version 3 (TRUC), 10,000 vB.** Kept instead of version 2 up to the 100,000 vB standard limit: at most 90 claims
  per transaction at 2-of-3, split into as many transactions as needed.
- **D8. Admin Wallet from Bitcoin Core.** The payout screens read the Admin Wallet from Bitcoin Core (`scantxoutset`,
  confirmed coins), not from the governance Admin Wallet panel, which needs an Electrum server; the design doc keeps
  the payout screen on Bitcoin Core only. Both use the same wallet, so the app shares coin reservations between them
  (`admin_wallet_reservations.rs`): the panel neither counts nor spends the fee coin of a pending payout and shows it
  as "held by pending payouts"; a new payout does not use a coin the panel is spending. The coins held by pending
  payouts are kept in the app data folder (`payout-reserved-coins.json`), because the panel also works without a payout
  session. A hold ends when its payout leaves Pending and, at the latest, when the payout expires. Only records that
  are payouts of this deployment can hold a coin.
- **D9. Incoming unconfirmed funds are not shown.** `scantxoutset` reports confirmed coins only, so the Admin Wallet
  dialog shows a received amount once it has one confirmation. The app's own unconfirmed sends are shown, because it
  remembers them (`payout-wallet-sends.json`). Showing unconfirmed receives would need a watch-only wallet inside
  Bitcoin Core. Deviation from PRD 06 §4.b.i and §4.b.ii ("net of unconfirmed send and receive").
- **D10. "Confirmed within 4 days" is read as "broadcast within 4 days".** PRD 06 §6.2 defines a pending transaction
  as one that is not broadcast yet and a past one as broadcast, so the expiry rule for pending transactions ends at
  broadcast. An unsigned import expires 4 days after it was stored.
- **D11. No P2PK destination.** PRD 06 §4.b.v lists P2PK among the destinations; it has no address format, so nothing
  can be typed for it. Every address type is accepted: P2PKH, P2SH, P2WPKH, P2WSH, P2TR.

## Admin Wallet dialog

The dialog on the Block Payouts screen is the Payout Administrator's Admin Wallet (PRD 06 §4.b), read from Bitcoin
Core and signed on the Ledger with the default policy `tr(@0/**)` (no registration).

- **Balance:** the total net of the app's own unconfirmed sends, that net amount, and what can be spent now.
- **Addresses:** each address with a balance. Coins held by a pending payout or being sent are marked.
- **Receive:** the first unused address as text and QR code, copy on click, "Show on Ledger". The dialog asks the node
  for its tip every 15 seconds and reads the wallet again at a new block, which rotates the address after a receive
  confirmed.
- **Send BTC:** destination, amount or Max, fee rate (0.1 to 10,000 sat/vB, default: the node's next-block estimate).
  The app builds the transaction, shows it with the Ledger's screens (Amount, To, Fees on 2.4.x and 2.5+), and signs
  only if it builds the same transaction again. Version 2, replaceable (`nSequence = 0xfffffffd`), change to the first
  unused change address, no change below 330 sat. Never spent: fee coins of pending payouts, coins the governance panel
  is sending, unconfirmed change.
- **Unconfirmed sends and fee bump:** each own unconfirmed send with "Bump fee" (BIP-125 replacement: same recipient
  and amount, change reduced; a Max send pays less to the recipient). The new fee must also cover the fees of
  transactions the replacement evicts. A replacement is recorded only once the node accepted it. Block Payouts
  transactions cannot be bumped (D6).
- A damaged `payout-wallet-sends.json` stops the wallet commands with a message and is left untouched.

## Tests

| Suite | What |
|-------|------|
| `cargo test -p payout-admin-core` | Report checks, PSBT build/sign/finalize via the bridge's `finalize_input`, signature lines, forged/duplicate signatures, merge/normalize, size model, planner, policy ⇄ connector equality. |
| `cargo test -p orchestrator-be payout` | Repository (memory), service (expiry, concurrency, broadcast report, refresh), routes with a mock node, session isolation (governance vs payout). |
| `cargo test -p desktop-app --lib payout` | Ledger policy conversion, descriptor parsing. |
| `cargo test -p alpen-multisig-e2e-tests --test e2e_payout_admin` | Regtest end to end with real Bitcoin Core, the real coordinator binary and the app layer, with a software signer in place of the Ledger. It covers sign-in, refusal of a non-admin, plan, confirm, sign to quorum, broadcast accepted by Core, Past unconfirmed → confirmed, rebroadcast, completion by pasted signatures with a forged line rejected, invalid reports ignored, and refusal to re-spend a spent connector. |
| `cargo test -p alpen-multisig-e2e-tests --test e2e_payout_multi_covenant` | Claims of three covenants in one transaction with two signers, Core accepts; the largest transaction (90 claims, 9,899 vB, the app's vsize equal to Core's) and the split of 91; an edited group over the limit refused; duplicate inputs marked across two admins' payouts; an imported PSBT completes a payout; a pending payout stays while its input is spent only in the mempool and is dropped once the spend is mined; the coordinator works with an unreachable ASM. |
| `e2e_payout_claims_matrix` (with `.local` runner) | One payout of N claims over 1–3 covenants on Speculos per device model and app version, or on a physical Ledger (`LEDGER_PHYSICAL=1`). Run 2026-11-13: Nano X and Nano S+, 2.4.6 and 2.5.1, 1–90 claims, all signed. |
| `cargo test -p alpen-multisig-e2e-tests --test e2e_payout_admin_speculos` | The same flow with admin 0 on Speculos (Bitcoin app 2.5.1, Nano S+), run when `LEDGER_SPECULOS_URL` is set: sign-in, policy registration with the on-device connector check, Confirm (claim and fee passes), and the Ledger signing another admin's pending payout. Core accepted the result. Skipped otherwise. |
| `npm run test:unit` (desktop-app) | Mapping, fee rate validation, expiry countdown, IPC argument wiring. |

## Not covered here

- **Physical Ledger.** Run by hand on 2026-11-13 on a Nano X (OS 2.7.0, Bitcoin Test 2.4.6, its own keys): sign-in,
  Show on Ledger, and payouts of 1, 4, 10, 20 and 90 claims signed and were accepted by Bitcoin Core. An earlier
  prototype's failure with 4 claims (`0x6a80`) did not reproduce. Not yet run on the physical device: several
  covenants, rejecting on the device, unplugging.
- **Postgres.** The coordinator's payout repository for Postgres, including the expiry table and the advisory locks,
  compiles and was reviewed but is not run by any test here; the tests use the in-memory repository.
- Open findings and requirement gaps of the 2026-11-13 audit are in `docs/assessment/` (local reports).
- **Unrelated finding, out of scope (noted as asked):** `staging/docker-compose.yml` (the `DATABASE_URL` of the
  orchestrator service, around line 96) contains a hard-coded hosted Postgres connection string with credentials.
  It should be rotated by its owner and moved to a secret. This change does not touch or use it.
