/**
 * Returns the balance copy for coins that pay the fee of a pending Block Payouts transaction — unspent
 * until that payout is broadcast or expires, and not available to any send here — or null when there are none.
 */
export function formatPayoutHeldBalanceLine(payoutReservedSats: number): string | null {
	if (!Number.isFinite(payoutReservedSats) || payoutReservedSats <= 0) return null
	return `${payoutReservedSats.toLocaleString()} sats held by pending payouts`
}
