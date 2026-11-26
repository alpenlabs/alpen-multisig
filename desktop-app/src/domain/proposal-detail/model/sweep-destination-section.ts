import type { DecodedProposalData } from '../hooks/use-decoded-proposal'

export function sweepDestinationSection({
	safeHarborChange,
	safeHarborProposed,
}: Pick<DecodedProposalData, 'safeHarborChange' | 'safeHarborProposed'>) {
	if (safeHarborChange !== null) {
		return { kind: 'change' as const, change: safeHarborChange }
	}

	if (safeHarborProposed !== null) {
		return { kind: 'proposed' as const, proposed: safeHarborProposed }
	}

	return null
}
