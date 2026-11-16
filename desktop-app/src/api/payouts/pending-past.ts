import { z } from 'zod'
import { payoutCall } from './call'

export const PendingPayoutInputSchema = z.object({
	outpoint: z.string(),
	/** Also an input of another pending transaction. */
	duplicate: z.boolean(),
})

export const PendingPayoutSchema = z.object({
	txid: z.string(),
	psbt: z.string(),
	createdBy: z.string(),
	expiresAt: z.number(),
	connectorCount: z.number(),
	inputs: PendingPayoutInputSchema.array(),
	hasDuplicates: z.boolean(),
	signaturesReceived: z.number(),
	signaturesNeeded: z.number(),
	signers: z
		.object({
			label: z.string(),
		})
		.array(),
	signedByMe: z.boolean(),
	complete: z.boolean(),
	broadcastError: z.string().nullable(),
	feeSat: z.number(),
	vsize: z.number(),
	/** The Ledger's name for each covenant wallet this transaction spends, e.g. `Payout 5df99e`. */
	policyNames: z.string().array(),
	changeAddress: z.string().nullable(),
	changeAddressOnDevice: z.string().nullable(),
	changeSat: z.number(),
})

export const PastPayoutSchema = z.object({
	txid: z.string(),
	rawTx: z.string(),
	status: z.union([z.literal('confirmed'), z.literal('unconfirmed')]),
	/** Block timestamp, Unix milliseconds. */
	blockTimestamp: z.number().nullable(),
	blockHeight: z.number().nullable(),
	connectorCount: z.number(),
	broadcastAt: z.number(),
})

export const PayoutsOverviewSchema = z.object({
	pending: PendingPayoutSchema.array(),
	past: PastPayoutSchema.array(),
	nodeWarning: z.string().nullable(),
})

export const BroadcastResultSchema = z.object({
	broadcast: z.boolean(),
	error: z.string().nullable(),
})

export const SubmitResultSchema = z.object({
	broadcast: BroadcastResultSchema.nullable(),
})

export const PasteResultSchema = z.object({
	added: z.number(),
	alreadyPresent: z.number(),
	/** The warning listing every invalid signature, when there was one. */
	message: z.string().nullable(),
	broadcast: BroadcastResultSchema.nullable(),
})

export type PendingPayoutInput = z.infer<typeof PendingPayoutInputSchema>

export type PendingPayout = z.infer<typeof PendingPayoutSchema>

export type PastPayout = z.infer<typeof PastPayoutSchema>

export type PayoutsOverview = z.infer<typeof PayoutsOverviewSchema>

export type BroadcastResult = z.infer<typeof BroadcastResultSchema>

export type SubmitResult = z.infer<typeof SubmitResultSchema>

export type PasteResult = z.infer<typeof PasteResultSchema>

export const payoutOverview = payoutCall('payout_overview', PayoutsOverviewSchema, () => undefined)

export const payoutSign = payoutCall('payout_sign', SubmitResultSchema, (txid: string) => ({ txid }))

export const payoutPasteSignatures = payoutCall(
	'payout_paste_signatures',
	PasteResultSchema,
	(txid: string, text: string) => ({ txid, text }),
)

export const payoutImport = payoutCall('payout_import', SubmitResultSchema, (text: string) => ({ text }))

export const payoutSignaturesText = payoutCall('payout_signatures_text', z.string(), (txid: string) => ({ txid }))

export const payoutRetryBroadcast = payoutCall('payout_retry_broadcast', BroadcastResultSchema, (txid: string) => ({
	txid,
}))

export const payoutRebroadcast = payoutCall('payout_rebroadcast', z.string(), (txid: string) => ({ txid }))
