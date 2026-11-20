import type { ReactNode } from 'react'

type AlertKind = 'error' | 'warning' | 'success'

const ALERT_KIND: Record<AlertKind, string> = {
	error: 'border-danger-border bg-danger-surface text-danger-deep',
	warning: 'border-accent-border bg-highlight-surface text-emphasis',
	success: 'border-[#a7f3d0] bg-[#ecfdf5] text-[#065f46]',
}

/** A message box that stays where the action is. With `onDismiss` it shows a close control. */
export function Alert({ kind, children, onDismiss }: { kind: AlertKind; children: ReactNode; onDismiss?: () => void }) {
	return (
		<div
			role={kind === 'error' ? 'alert' : 'status'}
			className={`my-2 flex items-start justify-between gap-3 rounded-lg border px-3 py-2 text-body-sm ${ALERT_KIND[kind]}`}
		>
			<div className="min-w-0 whitespace-pre-wrap wrap-anywhere">{children}</div>
			{onDismiss && (
				<button type="button" aria-label="Dismiss" className="shrink-0 px-1 leading-none" onClick={onDismiss}>
					×
				</button>
			)}
		</div>
	)
}

export function KeyValue({ label, mono = false, children }: { label: string; mono?: boolean; children: ReactNode }) {
	return (
		<div className="flex gap-3 py-0.5 text-body-sm">
			<span className="w-32 shrink-0 text-[#6b7280]">{label}</span>
			<span className={`min-w-0 wrap-anywhere ${mono ? 'font-mono text-label leading-5' : ''}`}>{children}</span>
		</div>
	)
}

/** A spinner with one line that says what the app is doing. */
export function Working({ label }: { label: string }) {
	return (
		<p role="status" className="m-0 flex items-center gap-2 text-body-sm font-medium text-[#0a0a0a]">
			<span
				className="block size-3.5 shrink-0 animate-spin rounded-full border-2 border-[#0a0a0a]/15 border-t-[#0a0a0a]"
				aria-hidden="true"
			/>
			{label}
		</p>
	)
}
