import { z } from 'zod'
import { payoutCall } from './call'
import { LedgerReviewSchema } from './checklist'

export const PayoutWalletSendSchema = z.object({
	txid: z.string(),
	toAddress: z.string(),
	amountSat: z.number(),
	feeSat: z.number(),

	feeRate: z.string(),
	/** Unix seconds. */
	sentAt: z.number(),
	status: z.union([z.literal('mempool'), z.literal('missing')]),
})

export const PayoutWalletSendReviewSchema = PayoutWalletSendSchema.omit({ sentAt: true, status: true }).extend({
	changeSat: z.number().nullable(),
	changeAddress: z.string().nullable(),
	review: LedgerReviewSchema,
})

export type PayoutWalletSend = z.infer<typeof PayoutWalletSendSchema>

export type PayoutWalletSendReview = z.infer<typeof PayoutWalletSendReviewSchema>

export type PayoutWalletSendInput = { address: string; amountSat: number | null; max: boolean; feeRate: string }

export type PayoutWalletBumpInput = { txid: string; feeRate: string }

export const payoutWalletFeeRate = payoutCall(
	'payout_wallet_fee_rate',
	z.union([z.string(), z.null()]),
	() => undefined,
)

export type PayoutWalletIntent = ({ kind: 'send' } & PayoutWalletSendInput) | ({ kind: 'bump' } & PayoutWalletBumpInput)

export const payoutWalletPrepare = payoutCall(
	'payout_wallet_prepare',
	PayoutWalletSendReviewSchema,
	(input: PayoutWalletIntent) => ({ input }),
)

export const payoutWalletExecute = payoutCall(
	'payout_wallet_execute',
	z.object({ txid: z.string() }),
	(input: PayoutWalletIntent, expectedTxid: string) => ({ input, expectedTxid }),
)
