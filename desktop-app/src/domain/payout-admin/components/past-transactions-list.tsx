import { useState } from 'react'
import { Paginator } from '@/components/paginator'
import type { PastBlockPayoutsTx } from '../model/payout-admin.types'
import { PAST_COLUMNS, PastTransactionRow } from './past-transaction-row'

type Props = {
	txs: PastBlockPayoutsTx[]
	onRebroadcast: (txId: string) => void
	onCopyRawTx: (txId: string) => void
}

const PAGE_SIZE = 10

export function PastTransactionsList({ txs, onRebroadcast, onCopyRawTx }: Props) {
	const [page, setPage] = useState(1)
	const totalPages = Math.ceil(txs.length / PAGE_SIZE)
	const paged = txs.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE)

	if (txs.length === 0) {
		return (
			<div className="rounded-xl border border-[#e5e7eb] bg-white px-6 py-12 text-center">
				<div className="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-full border border-[#e5e7eb] bg-bg-base text-[#9ca3af]">
					<svg width={18} height={18} viewBox="0 0 24 24" fill="none" aria-hidden>
						<rect x="3" y="3" width="18" height="18" rx="2" stroke="currentColor" strokeWidth="1.5" />
						<path d="M9 12h6M9 16h4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
					</svg>
				</div>
				<p className="m-0 font-display text-heading text-[#0a0a0a]">No past transactions</p>
				<p className="m-0 mt-1.5 text-body-sm text-[#6b7280]">Sent transactions will appear here.</p>
			</div>
		)
	}

	return (
		<div className="flex flex-col gap-3">
			<div className="rounded-xl border border-[#e5e7eb] bg-white">
				<div
					className={`${PAST_COLUMNS} px-5 py-2 text-mono-sm font-semibold uppercase tracking-wider text-[#9ca3af] border-b border-[#e5e7eb]`}
				>
					<span>Transaction ID</span>
					<span>Status</span>
					<span>Timestamp</span>
					<span />
				</div>
				{paged.map((tx) => (
					<PastTransactionRow
						key={tx.id}
						tx={tx}
						onRebroadcast={() => onRebroadcast(tx.id)}
						onCopyRawTx={() => onCopyRawTx(tx.id)}
					/>
				))}
			</div>
			<Paginator page={page} totalPages={totalPages} onPageChange={setPage} />
		</div>
	)
}
