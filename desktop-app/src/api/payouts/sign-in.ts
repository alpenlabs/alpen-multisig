import { z } from 'zod'
import { payoutCall } from './call'
import { LedgerReviewSchema } from './checklist'

export const PayoutAdminSchema = z.object({
	label: z.string(),
})

export const PayoutDeploymentSchema = z.object({
	network: z.string(),
	threshold: z.number(),
	admins: PayoutAdminSchema.array(),
})

export const PayoutSessionSchema = z.object({
	admin: PayoutAdminSchema,

	threshold: z.number(),

	/** The Bitcoin app on the Ledger, e.g. "Bitcoin Test 2.4.6". */
	ledgerApp: z.string(),
})

export type PayoutAdmin = z.infer<typeof PayoutAdminSchema>

export type PayoutDeployment = z.infer<typeof PayoutDeploymentSchema>

export type PayoutSession = z.infer<typeof PayoutSessionSchema>

export const payoutLoadDeployment = payoutCall('payout_load_deployment', PayoutDeploymentSchema, () => undefined)

/** Connects the Ledger and returns what it will show for the sign-in message. */
export const payoutSignInReview = payoutCall('payout_sign_in_review', LedgerReviewSchema, (baseUrl: string) => ({
	baseUrl,
}))

export const payoutSignIn = payoutCall('payout_sign_in', PayoutSessionSchema, (baseUrl: string) => ({ baseUrl }))

export const payoutSignOut = payoutCall(
	'payout_sign_out',
	z.null().transform(() => undefined),
	() => undefined,
)
