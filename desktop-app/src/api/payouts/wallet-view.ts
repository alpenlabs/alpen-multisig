import { z } from 'zod'
import { payoutCall } from './call'
import { PayoutWalletSendSchema } from './wallet-send'

export const PayoutAdminWalletCoinSchema = z.object({
	outpoint: z.string(),
	address: z.string(),
	valueSat: z.number(),

	branch: z.union([z.literal('receive'), z.literal('change')]),
	/** Already spent by a pending payout of this admin. */
	reserved: z.boolean(),
	/** Spent by a transaction of the Admin Wallet panel that is not on the network yet. */
	inFlight: z.boolean(),
})

export const PayoutAdminWalletSchema = z.object({
	unconfirmedSat: z.number(),
	addresses: z
		.object({
			address: z.string(),

			balanceSat: z.number(),
		})
		.array(),
	sends: PayoutWalletSendSchema.array(),
	tipHeight: z.number(),
	coins: PayoutAdminWalletCoinSchema.array(),
	totalSat: z.number(),
	availableSat: z.number(),
	receiveAddress: z.string(),
	receiveIndex: z.number(),
	/** The receive address as the Ledger shows it (regtest: tb1…), when it differs. */
	receiveOnDevice: z.string().nullable(),
})

export type PayoutAdminWalletCoin = z.infer<typeof PayoutAdminWalletCoinSchema>

/** The Admin Wallet as the payouts see it (confirmed coins on Bitcoin Core) and its next receive address. */
export type PayoutAdminWallet = z.infer<typeof PayoutAdminWalletSchema>

export const payoutAdminWallet = payoutCall('payout_admin_wallet', PayoutAdminWalletSchema, () => undefined)

/** Shows Admin Wallet receive address `index` on the Ledger; returns it once the device derived the same one. */
export const payoutShowReceiveAddress = payoutCall('payout_show_receive_address', z.string(), (index: number) => ({
	index,
}))

export const payoutWalletTip = payoutCall('payout_wallet_tip', z.number(), () => undefined)
