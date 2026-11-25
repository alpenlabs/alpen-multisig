import { Working } from './payout-ui'

/** The screens the Ledger shows, framed so the admin sees at once what to compare on the device. */
export function LedgerScreens({ screens }: { screens: string[] }) {
	return (
		<div className="my-4 overflow-hidden rounded-xl border-2 border-[#0a0a0a]">
			<h3 className="m-0 bg-[#0a0a0a] px-3 py-1.5 text-label font-semibold uppercase tracking-wider text-white">
				Compare these screens on your Ledger
			</h3>
			<ol className="m-0 list-none p-0">
				{screens.map((screen, i) => (
					<li key={i} className="flex gap-3 border-t border-[#e5e7eb] px-3 py-2 first:border-t-0">
						<span className="flex size-5 shrink-0 items-center justify-center rounded bg-[#f3f4f6] text-label font-semibold text-[#374151]">
							{i + 1}
						</span>
						<span className="min-w-0 whitespace-pre-wrap font-mono text-body leading-6 wrap-anywhere text-[#0a0a0a]">
							{screen}
						</span>
					</li>
				))}
			</ol>
		</div>
	)
}

export function Waiting() {
	return (
		<div>
			<Working label="Check each screen on your Ledger, then approve." />
			<p className="m-0 mt-1 text-body-sm text-[#6b7280]">
				Reject on the Ledger to cancel. Nothing changes here if you do.
			</p>
		</div>
	)
}
