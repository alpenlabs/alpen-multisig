import type { PastBlockPayoutsTx } from '../model/payout-admin.types'

export const PAST_COLUMNS = 'grid grid-cols-[minmax(0,1fr)_7rem_10rem_9rem] items-center gap-x-4'

type Props = {
	tx: PastBlockPayoutsTx
	onRebroadcast: () => void
	onCopyRawTx: () => void
}

function truncate(str: string, head = 10, tail = 8): string {
	if (str.length <= head + tail + 3) return str
	return `${str.slice(0, head)}…${str.slice(-tail)}`
}

function formatDate(d: Date): string {
	return d.toLocaleString(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
	})
}

export function PastTransactionRow({ tx, onRebroadcast, onCopyRawTx }: Props) {
	const isConfirmed = tx.status === 'confirmed'

	return (
		<div className="border-t border-[#f3f4f6] first:border-t-0">
			<div className={`${PAST_COLUMNS} px-5 py-2.5 text-body-sm`}>
				{/* TX ID */}
				<span title={tx.id} className="block truncate font-mono text-body-sm font-medium text-[#0a0a0a]">
					{truncate(tx.id, 12, 6)}
				</span>

				{/* Status badge */}
				<span
					className="inline-flex justify-self-start items-center gap-1.5 rounded-md border px-2 py-0.5 text-mono-sm font-medium"
					style={
						isConfirmed
							? { background: '#ecfdf5', color: '#059669', borderColor: '#a7f3d0' }
							: { background: '#eff6ff', color: '#2563eb', borderColor: '#bfdbfe' }
					}
				>
					<span
						className="h-1.5 w-1.5 flex-none rounded-full"
						style={{ background: isConfirmed ? '#059669' : '#2563eb' }}
						aria-hidden="true"
					/>
					{isConfirmed ? 'Confirmed' : 'Unconfirmed'}
				</span>

				{/* Timestamp */}
				<span className="text-label text-[#6b7280]">
					{isConfirmed && tx.blockTimestamp ? formatDate(tx.blockTimestamp) : ''}
				</span>

				{/* Actions */}
				<div className="flex justify-end items-center gap-1.5">
					{!isConfirmed && (
						<>
							<button
								type="button"
								className="inline-flex items-center gap-1.5 rounded-lg border border-[#e5e7eb] bg-white px-3 py-1.5 text-label font-medium text-[#374151] transition hover:border-[#d1d5db] hover:bg-[#f9fafb]"
								onClick={onRebroadcast}
							>
								<svg width={12} height={12} viewBox="0 0 24 24" fill="none" aria-hidden>
									<polyline
										points="1 4 1 10 7 10"
										stroke="currentColor"
										strokeWidth="1.5"
										strokeLinecap="round"
										strokeLinejoin="round"
									/>
									<path d="M3.51 15a9 9 0 1 0 .49-4.98" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
								</svg>
								Rebroadcast
							</button>
							<button
								type="button"
								title="Copy raw transaction"
								aria-label="Copy raw transaction"
								className="flex h-7 w-7 items-center justify-center rounded-md border border-[#e5e7eb] bg-white text-[#6b7280] transition hover:border-[#d1d5db] hover:bg-[#f9fafb] hover:text-[#374151]"
								onClick={onCopyRawTx}
							>
								<svg width={13} height={13} viewBox="0 0 24 24" fill="none" aria-hidden>
									<rect x="9" y="9" width="13" height="13" rx="2" stroke="currentColor" strokeWidth="1.5" />
									<path
										d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"
										stroke="currentColor"
										strokeWidth="1.5"
									/>
								</svg>
							</button>
						</>
					)}
				</div>
			</div>
		</div>
	)
}
