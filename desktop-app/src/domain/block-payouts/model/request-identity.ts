import type { PlanRequest } from '../../../api/payouts'

export function requestKey(request: PlanRequest): string {
	return JSON.stringify([request.claims, request.groups ?? null, request.feeRate])
}

export function isCurrentResponse(started: number, current: number): boolean {
	return started === current
}

/** Each edit, dispatch or cleanup supersedes responses owned by an older generation. */
export function createRequestGeneration() {
	let current = 0
	return {
		snapshot: () => current,
		next: () => ++current,
		isCurrent: (started: number) => isCurrentResponse(started, current),
	}
}
