/** An amount as the Ledger shows it (8 decimals, trailing zeros trimmed), then the satoshis in parentheses. */
export function formatAmount(sats: number, unit: string): string {
	const whole = Math.floor(sats / 100_000_000)
	const frac = String(sats % 100_000_000)
		.padStart(8, '0')
		.replace(/0+$/, '')
	const btc = frac === '' ? String(whole) : `${whole}.${frac}`
	return `${btc} ${unit} (${sats.toLocaleString('en-US')} sats)`
}
