import assert from 'node:assert/strict'

// The Rust commands bind arguments by name (camelCase on the JS side). A wrong key means the
// command body never runs, so pin every payout call's command name and argument shape.
const calls: Array<{ cmd: string; args: unknown }> = []
;(globalThis as unknown as { window: unknown }).window = {
	__TAURI_INTERNALS__: {
		invoke: (cmd: string, args: unknown) => {
			calls.push({ cmd, args })
			return Promise.resolve(null)
		},
	},
}

const api = await import('./payouts.ts')
const last = () => calls[calls.length - 1]

await api.payoutAdminWallet()
assert.deepEqual(last(), { cmd: 'payout_admin_wallet', args: {} })

await api.payoutShowReceiveAddress(3)
assert.deepEqual(last(), { cmd: 'payout_show_receive_address', args: { index: 3 } })

await api.payoutSignInReview('http://127.0.0.1:3000/api/v1')
assert.deepEqual(last(), { cmd: 'payout_sign_in_review', args: { baseUrl: 'http://127.0.0.1:3000/api/v1' } })

await api.payoutSignIn('http://127.0.0.1:3000/api/v1')
assert.deepEqual(last(), { cmd: 'payout_sign_in', args: { baseUrl: 'http://127.0.0.1:3000/api/v1' } })

const request = { claims: [{ outpoint: 'aa:1', covenantIndex: 0, payoutHeight: null }], feeRate: '2.5' }
await api.payoutPlan(request)
assert.deepEqual(last(), { cmd: 'payout_plan', args: { request } })

await api.payoutConfirm(request, 1234, ['tx1', 'tx2'])
assert.deepEqual(last(), {
	cmd: 'payout_confirm',
	args: { input: { request, expectedTotalFeeSat: 1234, expectedTxids: ['tx1', 'tx2'] } },
})

await api.payoutPasteSignatures('t1', 'line')
assert.deepEqual(last(), { cmd: 'payout_paste_signatures', args: { txid: 't1', text: 'line' } })

for (const [fn, cmd] of [
	[api.payoutSign, 'payout_sign'],
	[api.payoutSignaturesText, 'payout_signatures_text'],
	[api.payoutRetryBroadcast, 'payout_retry_broadcast'],
	[api.payoutRebroadcast, 'payout_rebroadcast'],
] as const) {
	await fn('t2')
	assert.deepEqual(last(), { cmd, args: { txid: 't2' } })
}

await api.payoutVerifyReports('{}')
assert.deepEqual(last(), { cmd: 'payout_verify_reports', args: { text: '{}' } })
await api.payoutImport('cHNidP8=')
assert.deepEqual(last(), { cmd: 'payout_import', args: { text: 'cHNidP8=' } })

console.log('payouts IPC wiring: ok')

for (const [fn, cmd] of [
	[api.payoutWalletTip, 'payout_wallet_tip'],
	[api.payoutWalletFeeRate, 'payout_wallet_fee_rate'],
] as const) {
	await fn()
	assert.deepEqual(last(), { cmd, args: {} })
}
const send = { kind: 'send' as const, address: 'destination', amountSat: 123, max: false, feeRate: '2.5' }
await api.payoutWalletPrepare(send)
assert.deepEqual(last(), { cmd: 'payout_wallet_prepare', args: { input: send } })
await api.payoutWalletExecute(send, 'reviewed')
assert.deepEqual(last(), { cmd: 'payout_wallet_execute', args: { input: send, expectedTxid: 'reviewed' } })
const bump = { kind: 'bump' as const, txid: 'original', feeRate: '3.5' }
await api.payoutWalletPrepare(bump)
assert.deepEqual(last(), { cmd: 'payout_wallet_prepare', args: { input: bump } })
await api.payoutWalletExecute(bump, 'replacement')
assert.deepEqual(last(), {
	cmd: 'payout_wallet_execute',
	args: { input: bump, expectedTxid: 'replacement' },
})

// Successful command invocation is not enough: reject malformed transport data before state updates.
const invalidWallet = await api.payoutAdminWallet()
assert.equal(invalidWallet.ok, false)
if (!invalidWallet.ok) assert.equal(invalidWallet.errorCode, 'ipc_schema_mismatch')
assert.deepEqual(await api.payoutSignOut(), { ok: true, data: undefined })
assert.deepEqual(await api.payoutWalletFeeRate(), { ok: true, data: null })
const { LedgerReviewSchema, PayoutWalletSendReviewSchema, VerifyReportsResultSchema } = await import('./payouts')
const review = { txids: ['reviewed'], steps: [], note: null, lockWarning: null }
assert.deepEqual(LedgerReviewSchema.parse(review), review)
assert.equal(LedgerReviewSchema.safeParse({ ...review, txids: [42] }).success, false)
const walletReview = {
	txid: 'reviewed',
	toAddress: 'destination',
	amountSat: 123,
	feeSat: 10,
	vsize: 100,
	feeRate: '0.1',
	changeSat: null,
	changeAddress: null,
	review,
}
const { vsize: _unreadVsize, ...displayReview } = walletReview
assert.deepEqual(PayoutWalletSendReviewSchema.parse(walletReview), displayReview)
assert.deepEqual(PayoutWalletSendReviewSchema.parse({ ...walletReview, vsize: _unreadVsize + 1 }), displayReview)
assert.equal(PayoutWalletSendReviewSchema.safeParse({ ...walletReview, review: null }).success, false)
assert.equal(VerifyReportsResultSchema.safeParse({ reports: [], malformed: [], tipHeight: 42 }).success, true)
assert.equal(
	VerifyReportsResultSchema.safeParse({ reports: [{ verdict: 'unknown' }], malformed: [], tipHeight: 42 }).success,
	false,
)
