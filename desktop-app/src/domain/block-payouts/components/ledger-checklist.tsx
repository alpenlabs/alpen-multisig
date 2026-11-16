import { PayoutError } from './payout-error'
import type { LedgerReview } from '@/api/payouts'
import { formatAmount } from '../model/block-payouts.mapping'

type Props = {
	review: LedgerReview | null
	error: string | null
	unit: string
}

/**
 * What the Ledger will show, screen by screen. The admin approves on the device only when it
 * matches this list exactly, and rejects otherwise.
 */
export function LedgerChecklist({ review, error, unit }: Props) {
	if (error) {
		return <PayoutError>Could not prepare what your Ledger will show: {error}</PayoutError>
	}
	if (!review) {
		return <p className="m-0 text-body-sm text-[#6b7280]">Preparing what your Ledger will show…</p>
	}
	return (
		<div className="flex flex-col gap-3">
			<p className="m-0 text-body-sm font-medium text-[#0a0a0a]">What your Ledger will show</p>
			<p className="m-0 text-label text-[#6b7280]">
				After you continue, approve on the Ledger only if every screen matches this list. If anything differs, reject on
				the device.
			</p>
			{review.note && (
				<p className="m-0 rounded-lg border border-accent-border bg-highlight-surface px-3 py-2 text-label text-emphasis">
					{review.note}
				</p>
			)}
			<ol className="m-0 flex max-h-80 list-none flex-col gap-3 overflow-y-auto p-0">
				{review.steps.map((step, i) => (
					<li key={`${i}:${step.title}`} className="rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3">
						<p className="m-0 mb-2 text-body-sm font-medium text-[#0a0a0a]">
							{i + 1}. {step.title}
						</p>
						{step.warning && (
							<p className="m-0 mb-2 rounded-lg border border-accent-border bg-highlight-surface px-3 py-2 text-label text-emphasis">
								{step.warning}
							</p>
						)}
						<dl className="m-0 flex flex-col gap-1.5">
							{step.details.map((d) => (
								<div key={d.label} className="flex flex-col gap-0.5">
									<dt className="text-label text-[#6b7280]">{d.label}</dt>
									{(d.sat !== null || d.value !== '') && (
										<dd className="m-0 whitespace-pre-wrap break-all font-mono text-label text-[#0a0a0a]">
											{d.sat === null ? d.value : formatAmount(d.sat, unit)}
										</dd>
									)}
									{d.note && <dd className="m-0 text-label text-[#6b7280]">{d.note}</dd>}
								</div>
							))}
						</dl>
					</li>
				))}
			</ol>
			{review.lockWarning && (
				<div className="rounded-lg border border-accent-border bg-highlight-surface px-3 py-2" role="alert">
					{review.lockWarning.split('\n').map((line, i) => (
						<p key={line} className={`m-0 text-label text-emphasis ${i === 0 ? 'font-semibold' : 'mt-1'}`}>
							{line}
						</p>
					))}
				</div>
			)}
		</div>
	)
}
