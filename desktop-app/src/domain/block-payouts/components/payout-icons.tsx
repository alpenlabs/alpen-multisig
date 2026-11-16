export function PayoutClockIcon({ size }: { size: number }) {
	return (
		<svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden>
			<circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="1.5" />
			<path d="M12 7v5l3 3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
		</svg>
	)
}

export function PayoutCopyIcon({ size }: { size: number }) {
	return (
		<svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden>
			<rect x="9" y="9" width="13" height="13" rx="2" stroke="currentColor" strokeWidth="1.5" />
			<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke="currentColor" strokeWidth="1.5" />
		</svg>
	)
}
