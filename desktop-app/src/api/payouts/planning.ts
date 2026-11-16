import { z } from 'zod'
import { payoutCall } from './call'

export const PlanClaimSchema = z.object({
	outpoint: z.string(),
	covenantIndex: z.number(),
	payoutHeight: z.number().nullable(),
})

export const PlannedTxSchema = z.object({
	claims: PlanClaimSchema.array(),
	feeCoins: z.object({ outpoint: z.string(), valueSat: z.number() }).array(),
	vsize: z.number(),
	feeSat: z.number(),
	changeSat: z.number(),
	errors: z.string().array(),
})

export const PayoutPlanSchema = z.object({
	txs: PlannedTxSchema.array(),

	feeRateLabel: z.string(),
	totalFeeSat: z.number(),
	ok: z.boolean(),
	availableSat: z.number(),
})

export type PlanClaim = z.infer<typeof PlanClaimSchema>

export type PlanRequest = { claims: PlanClaim[]; groups?: string[][]; feeRate: string }

export type PlannedTx = z.infer<typeof PlannedTxSchema>

export type PayoutPlan = z.infer<typeof PayoutPlanSchema>

export const payoutPlan = payoutCall('payout_plan', PayoutPlanSchema, (request: PlanRequest) => ({ request }))

/** Fee rate input: 0.1–10,000 sat/vB in steps of 0.1. Returns an error message, or null when valid. */
export function parseFeeRate(input: string): { value: number; error: 'format' | 'range' | null } {
	const s = input.trim()
	const value = Number(s)
	const error = !/^\d+(\.\d)?$/.test(s) ? 'format' : value < 0.1 || value > 10_000 ? 'range' : null
	return { value, error }
}

export function feeRateError(input: string): string | null {
	const { error } = parseFeeRate(input)
	if (error === 'format') return 'Enter a fee rate in sat/vB with at most one decimal (e.g. 2.5).'
	if (error === 'range') return 'The fee rate must be between 0.1 and 10,000 sat/vB.'
	return null
}
