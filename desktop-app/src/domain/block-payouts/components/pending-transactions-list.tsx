import { PayoutClockIcon } from './payout-icons'
import { usePayoutPagination } from '../hooks/use-payout-pagination'
import { Paginator } from '@/components/paginator'
import type { PendingPayoutTx } from '../model/block-payouts.types'
import { PendingTransactionCard } from './pending-transaction-card'

type Props = {
	txs: PendingPayoutTx[]
	unit: string
	hasConflicts: boolean
	busyTxId: string | null
	onRetryBroadcast: (txId: string) => void
	onSign: (txId: string) => void
	onPasteSignatures: (txId: string) => void
	onExport: (txId: string) => void
	onCopySignatures: (txId: string) => void
}

const PAGE_SIZE = 10

export function PendingTransactionsList({
	txs,
	unit,
	hasConflicts,
	busyTxId,
	onRetryBroadcast,
	onSign,
	onPasteSignatures,
	onExport,
	onCopySignatures,
}: Props) {
	const { page, totalPages, start, end, setPage } = usePayoutPagination(txs.length, PAGE_SIZE)
	const paged = txs.slice(start, end)

	if (txs.length === 0) {
		return (
			<div className="rounded-xl border border-[#e5e7eb] bg-white px-6 py-12 text-center">
				<div className="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-full border border-[#e5e7eb] bg-bg-base text-[#9ca3af]">
					<PayoutClockIcon size={18} />
				</div>
				<p className="m-0 font-display text-heading text-[#0a0a0a]">No pending transactions</p>
				<p className="m-0 mt-1.5 text-body-sm text-[#6b7280]">
					Block payouts from false claim reports, or import a transaction from another admin.
				</p>
			</div>
		)
	}

	return (
		<div className="flex flex-col gap-3">
			{hasConflicts && (
				<div className="flex items-start gap-2.5 rounded-xl border border-accent-border bg-highlight-surface px-4 py-3">
					<svg
						width={16}
						height={16}
						viewBox="0 0 24 24"
						fill="none"
						aria-hidden
						className="mt-0.5 shrink-0 text-emphasis-soft"
					>
						<circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="1.5" />
						<path d="M12 8v4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
						<circle cx="12" cy="16" r="1" fill="currentColor" />
					</svg>
					<p className="m-0 text-body-sm text-emphasis">
						Some pending transactions share inputs. See the <span className="font-medium">ⓘ icons</span> for details.
					</p>
				</div>
			)}
			{paged.map((tx) => (
				<PendingTransactionCard
					key={tx.txid}
					tx={tx}
					unit={unit}
					isBusy={busyTxId === tx.txid}
					onRetryBroadcast={() => onRetryBroadcast(tx.txid)}
					onSign={() => onSign(tx.txid)}
					onPasteSignatures={() => onPasteSignatures(tx.txid)}
					onExport={() => onExport(tx.txid)}
					onCopySignatures={() => onCopySignatures(tx.txid)}
				/>
			))}
			<Paginator page={page} totalPages={totalPages} onPageChange={setPage} />
		</div>
	)
}
