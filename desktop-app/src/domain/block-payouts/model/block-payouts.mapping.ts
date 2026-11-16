/** Remaining time until `expiresAt`, as shown on a pending card. */
export function formatTimeRemaining(expiresAt: number, now: number = Date.now()): { label: string; isUrgent: boolean } {
	const ms = expiresAt - now
	if (ms <= 0) return { label: 'Expired', isUrgent: true }
	const totalMinutes = Math.floor(ms / 60000)
	const hours = Math.floor(totalMinutes / 60)
	if (hours >= 24) return { label: `${Math.floor(hours / 24)}d ${hours % 24}h`, isUrgent: false }
	return { label: `${hours}h ${totalMinutes % 60}m`, isUrgent: true }
}

/** The unit the Ledger shows for amounts: BTC on mainnet, TEST on every other network. */
export function amountUnit(network: string): 'BTC' | 'TEST' {
	return network === 'main' ? 'BTC' : 'TEST'
}

/** An amount as the Ledger shows it (8 decimals, trailing zeros trimmed), then the satoshis in parentheses. */
export function formatAmount(sats: number, unit: string): string {
	const whole = Math.floor(sats / 100_000_000)
	const frac = String(sats % 100_000_000)
		.padStart(8, '0')
		.replace(/0+$/, '')
	const btc = frac === '' ? String(whole) : `${whole}.${frac}`
	return `${btc} ${unit} (${sats.toLocaleString('en-US')} sats)`
}

/** Groups of outpoints after the admin removed `removed` inputs; empty groups are dropped. */
export function groupsWithout(groups: string[][], removed: ReadonlySet<string>): string[][] {
	return groups.map((g) => g.filter((o) => !removed.has(o))).filter((g) => g.length > 0)
}
