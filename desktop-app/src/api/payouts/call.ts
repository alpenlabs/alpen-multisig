import type { z } from 'zod'
import { tauriCall } from '../tauri-bridge'

export function payoutCall<T, Args extends unknown[]>(
	command: string,
	schema: z.ZodType<T>,
	args: (...values: Args) => Record<string, unknown> | undefined,
) {
	return (...values: Args) => tauriCall(command, args(...values), schema)
}
