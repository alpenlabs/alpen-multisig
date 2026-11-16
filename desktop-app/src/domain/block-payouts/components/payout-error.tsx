import type { ReactNode } from 'react'

export function PayoutError({ children, spaced = false }: { children: ReactNode; spaced?: boolean }) {
	if (!children) return null
	return (
		<div
			className={`${spaced ? 'mt-4 ' : ''}rounded-xl border border-danger-border bg-danger-surface px-4 py-3`}
			role="alert"
		>
			<p className="m-0 whitespace-pre-wrap text-body-sm text-danger-deep">{children}</p>
		</div>
	)
}
