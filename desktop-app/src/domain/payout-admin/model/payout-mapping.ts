import type * as api from '@/api/payout-admin'
import type { PendingBlockPayoutsTx } from './payout-admin.types'

export function mapPending(tx: api.PayoutOverview['pending'][number], key: string): PendingBlockPayoutsTx {
	return {
		id: tx.txid,

		inputs: tx.claimInputs.map((outpoint) => ({
			outpoint,
			isConflicting: tx.duplicateOutpoints.includes(outpoint),
		})),

		claimInputCount: tx.claimInputCount,
		adminsSigned: tx.adminsSigned,
		signaturesRequired: tx.signaturesNeeded,
		signedByCurrentUser: tx.signedBy.includes(key),
		expiresAt: new Date(tx.expiresAtUnixSeconds * 1000),
		broadcastError: tx.broadcastError,
	}
}

export function feeRateTenths(raw: string): number | null {
	if (!/^\d+(\.\d)?$/.test(raw)) return null

	const value = Math.round(Number(raw) * 10)

	return value >= 1 && value <= 100000 ? value : null
}
