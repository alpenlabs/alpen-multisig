import assert from 'node:assert/strict'
import {
	parseWalletFeeRate,
	parseWalletAmount,
	walletSendInput,
	walletFormKey,
	canConfirmWallet,
	keepsReviewAfterFailure,
	shouldReloadWallet,
	walletCriticalError,
	type WalletForm,
} from '../wallet-send'

for (const raw of ['', '0', '-1', '10000.1', '1.11', '1e2', 'NaN']) assert.equal(parseWalletFeeRate(raw), null)
for (const [raw, rate] of [
	['0.1', '0.1'],
	['1', '1.0'],
	['2.5', '2.5'],
	['10000', '10000.0'],
	['0000000001.0', '1.0'],
])
	assert.equal(parseWalletFeeRate(raw), rate)
assert.equal(parseWalletAmount('0.00000001'), 1)
assert.equal(parseWalletAmount('1.23456789'), 123456789)
for (const raw of ['', '0', '-1', '1e2', '0.000000001', '9007199254740992']) assert.equal(parseWalletAmount(raw), null)
const form: WalletForm = { address: 'destination', amount: '0.001', feeRate: '2.5', max: false, bumpTxid: null }
assert.deepEqual(walletSendInput(form), {
	kind: 'send',
	address: 'destination',
	amountSat: 100000,
	feeRate: '2.5',
	max: false,
})
assert.equal(walletSendInput({ ...form, address: '' }), null)
assert.equal(walletSendInput({ ...form, amount: '' }), null)
assert.equal(walletSendInput({ ...form, feeRate: '' }), null)
const maxInput = walletSendInput({ ...form, max: true, amount: '' })
assert.equal(maxInput?.kind, 'send')
assert.equal(maxInput?.kind === 'send' ? maxInput.amountSat : undefined, null)
assert.equal(canConfirmWallet(form, null, false), false)
assert.equal(canConfirmWallet(form, walletFormKey(form), false), true)
assert.equal(canConfirmWallet(form, walletFormKey(form), true), false)
for (const patch of [{ address: 'new' }, { amount: '2' }, { feeRate: '3' }, { max: true }, { bumpTxid: 'tx' }]) {
	assert.equal(canConfirmWallet({ ...form, ...patch }, walletFormKey(form), false), false)
}
assert.equal(walletSendInput({ ...form, address: '', amount: '', bumpTxid: 'tx' })?.feeRate, '2.5')
assert.equal(shouldReloadWallet(2, 1, null, false, false), true)
assert.equal(shouldReloadWallet(0, 1, null, false, false), true)
assert.equal(shouldReloadWallet(1, 1, null, false, false), false)
// No wallet yet (the first load failed): read it at the next block.
assert.equal(shouldReloadWallet(2, undefined, null, false, false), true)
// A reload at this tip already ran and failed: wait for the next block instead of scanning on every poll.
assert.equal(shouldReloadWallet(2, 1, 2, false, false), false)
assert.equal(shouldReloadWallet(3, 1, 2, false, false), true)
assert.equal(shouldReloadWallet(2, 1, null, true, false), false)
assert.equal(shouldReloadWallet(2, 1, null, false, true), false)
// A rejection on the Ledger keeps the review; every other failure asks for a new one.
assert.equal(keepsReviewAfterFailure('rejected'), true)
assert.equal(keepsReviewAfterFailure('The transaction changed since you reviewed it. Review it again.'), false)
assert.equal(keepsReviewAfterFailure('Insufficient funds'), false)
for (const error of [
	'Destination must be a bitcoin address.',
	'Destination must be a mainnet bitcoin address.',
	'Destination must be a testnet bitcoin address.',
	'Insufficient funds',
	'other failure',
	'Rejected',
])
	assert.equal(walletCriticalError(error), error)
assert.equal(walletCriticalError('rejected'), null)
console.log('payout wallet validation, review identity, tip reload and errors: ok')
