import type { TransactionSigningProps } from '../hooks/use-transaction-review'
import { useState } from 'react'
import { Paginator } from '@/components/paginator'
import type { PendingBlockPayoutsTx } from '../model/payout-admin.types'
import { PENDING_COLUMNS, PendingTransactionRow } from './pending-transaction-row'
import { Alert } from './payout-ui'

type Props = {
	txs: PendingBlockPayoutsTx[]
	hasConflicts: boolean
	signingProps: TransactionSigningProps
	onPasteSignatures: (txId: string) => void
	onExport: (txId: string) => void
	onCopySignatures: (txId: string) => void
	onVerify: (txId: string) => void
	onRetry: (txId: string) => void
}

const PAGE_SIZE = 10

export function PendingTransactionsList(props: Props) {
	if (!props.txs.length) return <PendingSection {...props} />

	return (
		<>
			{props.hasConflicts && (
				<Alert kind="warning">
					Some pending transactions share a claim input (marked ⚠). Only one of them can confirm. Open Details to see
					which input.
				</Alert>
			)}
			{[true, false].map((verified) => {
				const txs = props.txs.filter((tx) => tx.verified === verified)

				return (
					<section key={String(verified)}>
						<h2 className="m-0 mb-2 mt-5 text-body font-semibold text-[#0a0a0a]">
							{verified ? 'Verified' : 'Unverified'}
						</h2>
						{txs.length ? (
							<PendingSection {...props} txs={txs} />
						) : (
							<p className="m-0 text-body-sm text-[#6b7280]">No {verified ? 'verified' : 'unverified'} transactions.</p>
						)}
					</section>
				)
			})}
		</>
	)
}

function PendingSection({
	txs,
	onVerify,
	signingProps,
	onPasteSignatures,
	onExport,
	onCopySignatures,
	onRetry,
}: Props) {
	const [page, setPage] = useState(1)
	const [open, setOpen] = useState<string | null>(null)
	const totalPages = Math.ceil(txs.length / PAGE_SIZE)
	const currentPage = Math.min(page, totalPages)
	const paged = txs.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE)

	if (txs.length === 0) {
		return (
			<div className="rounded-xl border border-[#e5e7eb] bg-white px-6 py-12 text-center">
				<div className="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-full border border-[#e5e7eb] bg-bg-base text-[#9ca3af]">
					<svg width={18} height={18} viewBox="0 0 24 24" fill="none" aria-hidden>
						<circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="1.5" />
						<path d="M12 7v5l3 3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
					</svg>
				</div>
				<p className="m-0 font-display text-heading text-[#0a0a0a]">No pending transactions</p>
				<p className="m-0 mt-1.5 text-body-sm text-[#6b7280]">
					Create a block payouts transaction to start collecting signatures.
				</p>
			</div>
		)
	}

	return (
		<div className="flex flex-col gap-3">
			<div className="rounded-xl border border-[#e5e7eb] bg-white">
				<div
					className={`${PENDING_COLUMNS} border-b border-[#e5e7eb] px-5 py-2 text-mono-sm font-semibold uppercase tracking-wider text-[#9ca3af]`}
				>
					<span>Time left</span>
					<span>Transaction ID</span>
					<span>Inputs</span>
					<span>Signatures</span>
					<span />
				</div>
				{paged.map((tx) => (
					<PendingTransactionRow
						key={tx.id}
						tx={tx}
						open={open === tx.id}
						onToggle={() => setOpen(open === tx.id ? null : tx.id)}
						signingProps={signingProps}
						onVerify={() => onVerify(tx.id)}
						onRetry={() => onRetry(tx.id)}
						onPasteSignatures={() => onPasteSignatures(tx.id)}
						onExport={() => onExport(tx.id)}
						onCopySignatures={() => onCopySignatures(tx.id)}
					/>
				))}
			</div>
			<Paginator page={currentPage} totalPages={totalPages} onPageChange={setPage} />
		</div>
	)
}
