import { truncate } from './modal-utils'
import { PayoutButton } from './payout-button'
import { PayoutCopyIcon } from './payout-icons'
import type { PastPayoutTx } from '../model/block-payouts.types'

type Props = {
	tx: PastPayoutTx
	isBusy: boolean
	onRebroadcast: () => void
	onCopyRawTx: () => void
}

/** Columns shared by the list header and every row: transaction, status, time, actions. */
export const PAST_GRID = 'grid grid-cols-[minmax(0,1fr)_8rem_11rem_10rem] items-center gap-4'

function formatDate(d: number): string {
	return new Date(d).toLocaleString(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
	})
}

export function PastTransactionRow({ tx, isBusy, onRebroadcast, onCopyRawTx }: Props) {
	const isConfirmed = tx.status === 'confirmed'

	return (
		<div className="rounded-xl border border-[#e5e7eb] bg-white">
			<div className={`${PAST_GRID} px-5 py-3.5`}>
				{/* TX ID */}
				<div className="min-w-0">
					<span className="font-mono text-body-sm font-medium text-[#0a0a0a]" title={tx.txid}>
						{truncate(tx.txid, 12, 6)}
					</span>
					<p className="m-0 mt-0.5 text-label text-[#6b7280]">
						{tx.connectorCount} claim input{tx.connectorCount !== 1 ? 's' : ''}
						{tx.blockHeight !== null ? ` · block ${tx.blockHeight.toLocaleString('en-US')}` : ''}
					</p>
				</div>

				{/* Status badge */}
				<span
					className="inline-flex items-center gap-1.5 justify-self-start rounded-md border px-2 py-0.5 text-mono-sm font-medium"
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
				<span className="text-right text-label text-[#6b7280]">
					{isConfirmed && tx.blockTimestamp !== null
						? formatDate(tx.blockTimestamp)
						: `Sent ${formatDate(tx.broadcastAt)}`}
				</span>

				{/* Actions */}
				<div className="flex items-center justify-end gap-1.5">
					{!isConfirmed && (
						<>
							<PayoutButton disabled={isBusy} appearance="rebroadcast" onClick={onRebroadcast}>
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
							</PayoutButton>
							<PayoutButton
								title="Copy transaction with signatures"
								aria-label="Copy transaction with signatures"
								appearance="icon"
								onClick={onCopyRawTx}
							>
								<PayoutCopyIcon size={13} />
							</PayoutButton>
						</>
					)}
				</div>
			</div>
		</div>
	)
}
