import { parseFeeRate, type PayoutWalletIntent } from '@/api/payouts'
import { parseAmountSats } from '@/domain/admin-wallet/model/send-validation'

export type WalletForm = { address: string; amount: string; max: boolean; feeRate: string; bumpTxid: string | null }

export function parseWalletFeeRate(raw: string): string | null {
	const { value, error } = parseFeeRate(raw)
	if (error) return null
	// One spelling per rate ("007" and "7" are both "7.0"), so the backend never sees a form it refuses.
	return value.toFixed(1)
}

/** BTC entry is converted with integer arithmetic, with no rounding of fractional satoshis. */
export function parseWalletAmount(raw: string): number | null {
	const value = raw.trim()
	if (!/^\d+(\.\d{1,8})?$/.test(value)) return null
	const [whole, fraction = ''] = value.split('.')
	const sats = parseAmountSats(whole + fraction.padEnd(8, '0'))
	return sats !== null && sats > 0 ? sats : null
}

export function walletSendInput(form: WalletForm): PayoutWalletIntent | null {
	const feeRate = parseWalletFeeRate(form.feeRate)
	const amountSat = parseWalletAmount(form.amount)
	if (!feeRate || (!form.bumpTxid && (!form.address.trim() || (!form.max && amountSat === null)))) return null
	return form.bumpTxid
		? { kind: 'bump', txid: form.bumpTxid, feeRate }
		: { kind: 'send', address: form.address.trim(), amountSat: form.max ? null : amountSat, max: form.max, feeRate }
}

export function walletFormKey(form: WalletForm): string {
	return JSON.stringify(form)
}

export function canConfirmWallet(form: WalletForm, validatedKey: string | null, busy: boolean): boolean {
	return !busy && walletSendInput(form) !== null && validatedKey === walletFormKey(form)
}

/**
 * Whether a new chain tip calls for reading the wallet again. `attemptedTip` is the tip of the last reload that was
 * started: a reload that failed is tried again at the next block, not on every poll (each one scans the UTXO set).
 */
export function shouldReloadWallet(
	tip: number,
	walletTip: number | undefined,
	attemptedTip: number | null,
	loading: boolean,
	ledgerBusy: boolean,
) {
	return tip !== walletTip && tip !== attemptedTip && !loading && !ledgerBusy
}

/** A Ledger rejection leaves the review as it is; after any other failure the wallet may have changed. */
export function keepsReviewAfterFailure(error: string): boolean {
	return error === 'rejected'
}

/** All backend failures are critical and verbatim; a Ledger rejection has no visible error. */
export function walletCriticalError(error: string): string | null {
	return error === 'rejected' ? null : error
}
