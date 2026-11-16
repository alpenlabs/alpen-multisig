import type { LedgerReview, PastPayout, PendingPayout, VerifiedReport } from '@/api/payouts'

export type PendingPayoutTx = PendingPayout
export type PastPayoutTx = PastPayout
export type ReportRow = VerifiedReport

export type PayoutSignIn =
	| { phase: 'idle' }
	| { phase: 'loading' }
	/** The Ledger is connected; it will show `review` when it signs the sign-in message. */
	| { phase: 'review'; review: LedgerReview }
	| { phase: 'signing'; review: LedgerReview }
	| { phase: 'error'; message: string }
	| {
			phase: 'signed-in'
			adminLabel: string
			threshold: number
			adminCount: number
			network: string
			ledgerApp: string
	  }

export type ActiveModal =
	| { kind: 'sign'; txId: string }
	| { kind: 'paste'; txId: string }
	| { kind: 'import' }
	| { kind: 'create' }
	| { kind: 'admin-wallet' }
	| null

export type Tab = 'pending' | 'past'

export type Toast = { kind: 'ok' | 'error'; message: string }
