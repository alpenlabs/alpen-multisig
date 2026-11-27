export type BlockPayoutsInput = { outpoint: string; isConflicting: boolean }

export type PendingBlockPayoutsTx = {
	id: string
	verified: boolean
	reason: string | null
	inputs: BlockPayoutsInput[]
	claimInputCount: number
	adminsSigned: number
	signaturesRequired: number
	signedByCurrentUser: boolean
	expiresAt: Date
	broadcastError: string | null
}

export type PastBlockPayoutsTx = {
	id: string
	status: 'unconfirmed' | 'confirmed'
	blockTimestamp?: Date
	rawTx: string
}

// 'create' replaces the lists with the Block payouts page; the other two are dialogs.
export type ActiveModal = { kind: 'paste' | 'verify'; txId: string } | { kind: 'create' } | { kind: 'import' } | null
