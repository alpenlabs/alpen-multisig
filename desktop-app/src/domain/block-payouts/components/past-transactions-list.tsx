import { usePayoutPagination } from '../hooks/use-payout-pagination'
import { Paginator } from '@/components/paginator'
import type { PastPayoutTx } from '../model/block-payouts.types'
import { PAST_GRID, PastTransactionRow } from './past-transaction-row'

type Props = {
	txs: PastPayoutTx[]
	busyTxId: string | null
	onRebroadcast: (txId: string) => void
	onCopyRawTx: (txId: string) => void
}

const PAGE_SIZE = 10

export function PastTransactionsList({ txs, busyTxId, onRebroadcast, onCopyRawTx }: Props) {
	const { page, totalPages, start, end, setPage } = usePayoutPagination(txs.length, PAGE_SIZE)
	const paged = txs.slice(start, end)

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
				<p className="m-0 mt-1.5 text-body-sm text-[#6b7280]">Broadcast transactions will appear here.</p>
			</div>
		)
	}

	return (
		<div className="flex flex-col gap-2.5">
			<div className={`${PAST_GRID} px-5 pb-1`}>
				<span className="text-mono-sm font-semibold uppercase tracking-wider text-[#9ca3af]">Transaction ID</span>
				<span className="text-mono-sm font-semibold uppercase tracking-wider text-[#9ca3af]">Status</span>
				<span className="text-right text-mono-sm font-semibold uppercase tracking-wider text-[#9ca3af]">
					Block time
				</span>
				<span />
			</div>
			{paged.map((tx) => (
				<PastTransactionRow
					key={tx.txid}
					tx={tx}
					isBusy={busyTxId === tx.txid}
					onRebroadcast={() => onRebroadcast(tx.txid)}
					onCopyRawTx={() => onCopyRawTx(tx.txid)}
				/>
			))}
			<Paginator page={page} totalPages={totalPages} onPageChange={setPage} />
		</div>
	)
}
