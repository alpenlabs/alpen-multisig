export type BlockPayoutsInput = { outpoint: string; isConflicting: boolean }

export type PendingBlockPayoutsTx = {
	id: string
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

export type ActiveModal = { kind: 'paste'; txId: string } | { kind: 'create' } | null
