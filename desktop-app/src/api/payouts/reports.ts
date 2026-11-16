import { z } from 'zod'
import { payoutCall } from './call'

export const FalseClaimReportSchema = z.object({
	newClaimTxid: z.string(),
})

export const VerifiedReportSchema = z.object({
	newClaimTxid: z.string(),
	verdict: z.union([z.literal('valid'), z.literal('invalid'), z.literal('spent')]),
	reasons: z.string().array(),
	outpoint: z.string(),
	covenantIndex: z.number().nullable(),

	payoutHeight: z.number().nullable(),

	inPending: z.boolean(),
})

export const VerifyReportsResultSchema = z.object({
	reports: VerifiedReportSchema.array(),
	malformed: z
		.object({
			position: z.number(),
			error: z.string(),
		})
		.array(),
})

export type FalseClaimReport = z.infer<typeof FalseClaimReportSchema>

export type VerifiedReport = z.infer<typeof VerifiedReportSchema>

export type VerifyReportsResult = z.infer<typeof VerifyReportsResultSchema>

export const payoutVerifyReports = payoutCall('payout_verify_reports', VerifyReportsResultSchema, (text: string) => ({
	text,
}))
