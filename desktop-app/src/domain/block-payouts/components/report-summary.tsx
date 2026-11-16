import type { ReportRow } from '../model/block-payouts.types'
import { truncate } from './modal-utils'
export function ReportSummary({
	reports,
	ignoredCount,
	malformed,
}: {
	reports: ReportRow[]
	ignoredCount: number
	malformed: string[]
}) {
	const ignored = reports.filter((r) => r.verdict !== 'valid')
	const valid = reports.length - ignored.length
	return (
		<div className="flex flex-col gap-2">
			<p className="m-0 text-body-sm text-[#374151]">
				{valid} valid report{valid !== 1 ? 's' : ''}
				{ignoredCount + malformed.length > 0 ? `, ${ignoredCount + malformed.length} ignored` : ''}.
			</p>
			{(ignored.length > 0 || malformed.length > 0) && (
				<details className="rounded-xl border border-danger-border bg-danger-surface px-4 py-2">
					<summary className="cursor-pointer text-body-sm font-medium text-danger-deep">Ignored reports</summary>
					<ul className="m-0 mt-2 flex list-none flex-col gap-1.5 p-0">
						{malformed.map((m) => (
							<li key={m} className="text-label text-danger-deep">
								{m}
							</li>
						))}
						{ignored.map((r) => (
							<li key={r.outpoint} className="text-label text-danger-deep">
								<span className="font-mono">{truncate(r.newClaimTxid)}</span>:{' '}
								{r.verdict === 'spent' ? 'already spent' : r.reasons.join('; ')}
							</li>
						))}
					</ul>
				</details>
			)}
		</div>
	)
}
