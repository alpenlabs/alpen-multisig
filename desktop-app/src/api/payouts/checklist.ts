import { z } from 'zod'
import { payoutCall } from './call'
import type { PlanRequest } from './planning'

export const LedgerDetailSchema = z.object({
	/** The label as the device shows it. */
	label: z.string(),
	value: z.string(),
	sat: z.number().nullable(),
	/** Context only the app can add (whose key it is); not on the device. */
	note: z.string().nullable(),
})

export const LedgerStepSchema = z.object({
	title: z.string(),
	/** A device warning that is expected at this step, with why it is safe to continue. */
	warning: z.string().nullable(),
	details: LedgerDetailSchema.array(),
})

export const LedgerReviewSchema = z.object({
	txids: z.string().array(),
	steps: LedgerStepSchema.array(),
	/** Set when the Ledger's app is newer than the versions whose screens were checked. */
	note: z.string().nullable(),
	/** Set when a transaction has so many claims that the Ledger may lock while it works. */
	lockWarning: z.string().nullable(),
})

/** One line the Ledger shows. Amounts carry `sat` (and an empty `value`). */
export type LedgerDetail = z.infer<typeof LedgerDetailSchema>

/** One interaction with the Ledger, in the order it happens. */
export type LedgerStep = z.infer<typeof LedgerStepSchema>

export type LedgerReview = z.infer<typeof LedgerReviewSchema>

/** What the Ledger will show when Confirm signs the planned transactions. */
export const payoutReviewConfirm = payoutCall('payout_review_confirm', LedgerReviewSchema, (request: PlanRequest) => ({
	request,
}))
