import assert from 'node:assert/strict'
import { mapPending, feeRateTenths } from '../model/payout-mapping'

const pending = {
	txid: 'tx',
	psbt: '',
	claimInputCount: 2,
	claimInputs: ['claim:1', 'second:1'],
	duplicateOutpoints: ['claim:1'],
	signedBy: ['admin'],
	adminsSigned: 1,
	signaturesNeeded: 2,
	expiresAtUnixSeconds: 90,
	broadcastError: 'offline',
}
const mapped = mapPending(pending, 'admin')
assert.equal(mapped.claimInputCount, 2)
assert.equal(mapped.adminsSigned, 1)
assert.equal(mapped.signaturesRequired, 2)
assert.equal(mapped.signedByCurrentUser, true)
assert.equal(mapped.broadcastError, 'offline')
assert.equal(mapped.expiresAt.getTime(), 90000)
assert.equal(mapPending(pending, 'other').signedByCurrentUser, false)
assert.deepEqual(
	mapped.inputs.map((input) => [input.outpoint, input.isConflicting]),
	[
		['claim:1', true],
		['second:1', false],
	],
)
assert.equal(
	mapPending({ ...pending, duplicateOutpoints: [] }, '').inputs.some((input) => input.isConflicting),
	false,
)
for (const [raw, expected] of [
	['0.1', 1],
	['10000', 100000],
	['2.3', 23],
	['0', null],
	['10000.1', null],
	['1.01', null],
	['1e2', null],
	['', null],
] as const) {
	assert.equal(feeRateTenths(raw), expected)
}

// Route and shared-wallet contracts, following the existing source-wiring tests.
const { readFileSync } = await import('node:fs')
const read = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')
const screen = read('../../../screens/payout-admin-screen.tsx')
assert.match(screen, /<WalletSessionControl\s+hideAdminId\s+hideSessionTime/)
assert.doesNotMatch(screen, /Back to multisig selection/)
assert.match(screen, /<DisconnectButton onClick=\{\(\) => void handleDisconnect\(\)\}/)
assert.doesNotMatch(read('../../../screens/wallet-connect-screen.tsx'), /Payout Administrator \(Ledger\)/)
assert.match(read('../../admin-wallet/components/wallet-session-control.tsx'), /hideAdminId=\{hideAdminId\}/)
assert.match(
	read('../components/payout-sign-in-panel.tsx'),
	/<KeyValue label="Expected address" mono>\s*\{identity.expectedAddress\}/,
)

const app = read('../../../../src-tauri/src/application/payout_admin.rs')
const importing = app.slice(app.indexOf('pub async fn import_psbt('), app.indexOf('pub async fn paste_signatures('))
assert.ok(
	importing.indexOf('check_change_output(deployment, &psbt)?') < importing.indexOf('state.signed_in_coordinator()?'),
)
const bundling = app.slice(
	app.indexOf('pub async fn bundle_claims('),
	app.indexOf('pub async fn review_block_payouts_tx'),
)
assert.match(bundling, /release_stale_reservations\(/)
assert.ok(
	bundling.indexOf('release_stale_reservations(') <
		bundling.indexOf('let reserved = wallet_service.reserved_outpoints()'),
)
assert.doesNotMatch(bundling, /pending_payout\.expires_at/)
const overview = app.slice(
	app.indexOf('pub async fn pending_and_past('),
	app.indexOf('pub async fn copy_transaction_text('),
)
assert.match(overview, /release_stale_reservations\(/)
const wallet = read('../../../../src-tauri/src/commands/admin_wallet.rs')
const bump = wallet.slice(
	wallet.indexOf('pub async fn admin_wallet_bump_fee('),
	wallet.indexOf('pub async fn admin_wallet_estimate_send'),
)
assert.ok(bump.indexOf('refresh_own_pending_reservations().await?') < bump.indexOf('.bump_fee('))
