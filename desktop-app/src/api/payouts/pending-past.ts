import { z } from 'zod'
export const BroadcastResultSchema = z.object({
	broadcast: z.boolean(),
	error: z.string().nullable(),
})

export const SubmitResultSchema = z.object({
	broadcast: BroadcastResultSchema.nullable(),
})

