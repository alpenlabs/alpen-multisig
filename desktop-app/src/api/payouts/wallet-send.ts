import { z } from 'zod'
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
