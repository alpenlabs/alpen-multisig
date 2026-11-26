import type { Proposal } from '@/api/proposals'

function normalizeHex(hex: string): string {
	return hex.replace(/^0x/i, '').toLowerCase()
}

export function cancelMatchesProposal(
	stored: Pick<Proposal, 'targetActionId' | 'actionHex'>,
	actionId: string,
	expectedHex: string,
): boolean {
	return stored.targetActionId === actionId && normalizeHex(stored.actionHex) === normalizeHex(expectedHex)
}
