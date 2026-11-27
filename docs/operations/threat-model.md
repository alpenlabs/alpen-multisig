# Threat model (P-051) — summary

**SSOT (security — assets & risks):** Pair with [`specs/signer-safety-model.md`](../specs/signer-safety-model.md) for signer UX principles. This doc covers threats and mitigations; signer-safety covers what the signer must verify. Read both; do not duplicate.

## Assets

- Signer private keys (HW wallet, mnemonic dev path, operator key for broadcast)
- Session bearer tokens (authority-scoped)
- Proposal `action_hex` and collected signatures

## Trust boundaries

The configured ASM endpoint is trusted for the N/N key history used to verify false-claim reports.
Each signer checks a version 2 report against their own Electrum server during review and again before signing.
The report must cover every claim input exactly. A confirmed ACK establishes fault for one operator under PRD 07,
so that operator’s claims may precede or follow the ACK.
We link operators by equal claim output 3 (CPFP) scripts instead of the contest proof output suggested by PRD 07
because output 3 needs no search over deposit indexes and the contest’s N/N signature covers the claim txid and
thus all its outputs.
Transactions are grouped by operator and ACK evidence before size splitting. This can produce more transactions
than PRD 06’s size-only rule. The operating assumption is that one operator turns faulty at a time.

### Block payouts invariants

- **Signing:** the app asks the Ledger to sign a block payouts transaction only if, in the same signing call, its
  report covers the transaction’s claim inputs exactly and every claim passes the check against the signer’s own
  Electrum server. The inputs are checked against the chain at the same time. This holds for the signer’s own
  transactions and for pending ones.
- **Verified:** a pending transaction shown as verified has a report that covers its claim inputs exactly (checked
  on every list load), and every claim passed the Electrum check once this session. It does not promise that the
  transaction can still confirm; the coordinator removes pending transactions whose inputs are spent in a
  confirmed transaction.
- **Stored reports:** the app attaches only reports cut down to exactly the transaction’s claims. The coordinator
  stores at most one report per transaction, never replaces it, and never verifies it.
- **Coordinator:** a stored report is only a candidate. No signer treats it as verified before checking it.

```mermaid
flowchart LR
  React[React webview]
  Tauri[Tauri Rust]
  Orch[Orchestrator]
  BTC[Bitcoin RPC]
  ASM[ASM RPC]

  React -->|IPC no secrets in prod| Tauri
  Tauri -->|HTTPS| Orch
  Tauri -->|RPC| BTC
  Orch -->|RPC| ASM
```

## Top risks (Wave 2 mitigations)

| Risk | Mitigation |
|------|------------|
| Malicious backend returns wrong proposal | P-005 hash verify (Track F) |
| Well-known test mnemonic in production | R1.1 per-signer capability: the software mnemonic signer is rejected on mainnet (`allowed_on` = regtest/testnet only), only a hardware signer is allowed on mainnet; the mnemonic-signing IPC is off in release builds (`ALLOW_DEV_MNEMONIC_SIGNING` / `dev_secrets.rs`, P-040). Reveal key is a per-broadcast ephemeral key (R1.0) |
| Mnemonic / raw key exfil via XSS | P-003 + P-040: dev signing IPC off in release (Decision #2; Track A) |
| Supply-chain compromise | P-011 audit/deny/lockfile |
| Cross-authority data leak | P-002 session + proposal scope |
| Coordinator/UI desync on broadcast | P-066 desktop execute + PATCH metadata |
| UTXO double-selection: two commits (or a commit and a send) spend the same coin, so one bundle is replaced and its reveal — whose envelope key is already evicted — can never land (#516) | In-memory reservation of every built tx's inputs before the wallet lock is released; a failed broadcast is settled from the broadcaster's typed answer, and what stays open (ambiguous, undelivered, reveal missing) by the desktop settle loop: found → recorded, absent at every source for 3 checks over 10 min → released and `failed`, commit live + reveal missing → reveal resubmitted. `claim_broadcast` also returns 409 while another approved proposal of that authority is in flight. A proposal that is no longer approved does not count. A `commit_broadcasted` row with no txids older than 10 minutes can be re-claimed and does not block the authority; a row that already has a txid cannot. The desktop offers send again on that empty claim only once it is past the window (`broadcast_claim_stale`, on the server clock); inside it, the row reads in flight to every signer. Documented limits: reservations and absence counters are in memory (an app restart forgets them); BDK 1.2 cannot evict a tx, so a recorded tx dropped without a conflict keeps its inputs looking spent until the session is rebuilt; a commit chained on unconfirmed change dies with its parent. See `specs/proposal-broadcast-commit-reveal.md` |

## Accepted risks

- **Removed signer keeps offchain access until session expiry (#582).** A session bearer token is valid for up to 24 hours from sign-in, and membership is checked only at `/auth/verify`. A signer removed from the multisig during that window (or anyone holding a leaked token) can keep reading proposals and calling every write endpoint. The orchestrator is coordination only and does not verify governance signatures: it checks that `signer_pubkey` matches the session and stores `signature_hex` as given, so such a caller can open proposals, take a `seq_no`, or add an approval that makes a proposal look ready to broadcast. Mitigation: the ASM rejects signatures from non-members at enactment, so none of this can take effect onchain; the exposure is misleading coordination state, not unauthorized governance. Follow-up: re-check membership on write endpoints.

## Out of scope (Wave 3+)

- Signed releases all platforms (P-011 full)
- Shared types codegen (P-043)
- Event-sourced audit log (P-031)
