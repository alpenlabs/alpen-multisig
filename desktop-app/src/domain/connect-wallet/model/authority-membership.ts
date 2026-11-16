import { AuthRole } from '@/types'

// Roles not backed by ASM state: their availability does not come from the ASM membership check.
const ASM_EXEMPT_ROLES = new Set<AuthRole>([AuthRole.PayoutAdministrator])

/**
 * Whether an authority's availability is decided by the ASM membership check. Payout
 * Administrators are not: they must be selectable while that check runs, and when the ASM cannot
 * be reached at all.
 */
export function needsMembershipCheck(role: AuthRole | null): boolean {
	return role !== null && !ASM_EXEMPT_ROLES.has(role)
}
