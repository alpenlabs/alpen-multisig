import { useState } from 'react'
import type { PayoutAdminHook, Tab } from '../hooks/use-payout-admin'
import { PendingTransactionsList } from './pending-transactions-list'
import { PastTransactionsList } from './past-transactions-list'
import { PasteSignaturesModal } from './paste-signatures-modal'
import { CreateBlockPayoutsModal } from './create-block-payouts-modal'

const TAB_LABELS: { id: Tab; label: string }[] = [
	{ id: 'pending', label: 'Pending' },
	{ id: 'past', label: 'Past' },
]

export function PayoutAdminDashboard({ hook }: { hook: PayoutAdminHook }) {
	const {
		activeTab,
		pendingTxs,
		pastTxs,
		hasConflicts,
		activeModal,
		toast,
		setActiveTab: onTabChange,
		openSignModal,
		openPasteModal,
		openCreateModal,
		closeModal,
		handlePasteSignatures,
		handleExport,
		handleCopySignatures,
		handleRebroadcast,
		handleCopyRawTx,
	} = hook
	const pasteTxId = activeModal?.kind === 'paste' ? activeModal.txId : null
	const [psbt, setPsbt] = useState('')

	return (
		<section className="mx-auto w-full max-w-200">
			{/* Page header */}
			<div className="mb-6 flex items-end justify-between gap-4">
				<div>
					<h1 className="m-0 font-display text-display-md font-normal leading-[1.2] tracking-[-0.005em] text-[#0a0a0a]">
						Payout Admin
					</h1>
					<p className="m-0 mt-1 text-body-sm leading-normal text-[#6b7280]">
						Manage block payouts transactions for the Payout Administrator multisig.
					</p>
				</div>
				<button
					type="button"
					className="inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body font-medium text-white transition hover:bg-[#2a2a2a] active:scale-[0.98]"
					onClick={openCreateModal}
				>
					<span aria-hidden="true">+</span>
					Block payouts
				</button>
			</div>

			<label className="block text-body-sm font-medium text-[#374151]">
				Import base64 PSBT
				<textarea
					className="mt-1 block w-full rounded-xl border border-[#e5e7eb] bg-white p-3 font-mono text-body-sm"
					value={psbt}
					onChange={(event) => setPsbt(event.target.value)}
				/>
			</label>
			<button
				type="button"
				className="mb-5 mt-2 rounded-lg border border-[#e5e7eb] bg-white px-3 py-1.5 text-label font-medium text-[#374151] hover:bg-[#f9fafb] disabled:opacity-50"
				disabled={!psbt.trim()}
				onClick={() => void hook.handleImport(psbt)}
			>
				Import PSBT
			</button>
			{hook.stale && <p role="status">Pending and Past numbers are stale. The coordinator refresh failed.</p>}
			{/* Tabs */}
			<div className="mb-5 flex gap-0.5 rounded-xl border border-[#e5e7eb] bg-bg-base p-1">
				{TAB_LABELS.map(({ id, label }) => (
					<button
						key={id}
						type="button"
						className="flex-1 rounded-lg py-2 text-body-sm font-medium transition"
						style={
							activeTab === id
								? { background: '#fff', color: '#0a0a0a', boxShadow: '0 1px 3px rgba(0,0,0,0.08)' }
								: { background: 'transparent', color: '#6b7280' }
						}
						onClick={() => onTabChange(id)}
					>
						{label}
						{id === 'pending' && pendingTxs.length > 0 && (
							<span
								className="ml-1.5 rounded-full px-1.5 py-0.5 text-[10px] font-semibold"
								style={
									activeTab === 'pending'
										? { background: '#0a0a0a', color: '#fff' }
										: { background: '#e5e7eb', color: '#6b7280' }
								}
							>
								{pendingTxs.length}
							</span>
						)}
					</button>
				))}
			</div>

			{activeTab === 'pending' && 0 < hook.hidden && (
				<p role="status">
					{hook.hidden} pending block payouts transaction(s) do not match your params.toml and are hidden.
				</p>
			)}
			{/* Tab content */}
			{activeTab === 'pending' ? (
				<PendingTransactionsList
					txs={pendingTxs}
					hasConflicts={hasConflicts}
					onSign={openSignModal}
					onRetry={hook.handleRetry}
					onPasteSignatures={openPasteModal}
					onExport={handleExport}
					onCopySignatures={handleCopySignatures}
				/>
			) : (
				<PastTransactionsList txs={pastTxs} onRebroadcast={handleRebroadcast} onCopyRawTx={handleCopyRawTx} />
			)}

			{/* Modals */}

			{activeModal?.kind === 'paste' && pasteTxId && (
				<PasteSignaturesModal txId={pasteTxId} onSubmit={handlePasteSignatures} onClose={closeModal} />
			)}

			{activeModal?.kind === 'create' && <CreateBlockPayoutsModal hook={hook} />}

			{/* Toast */}
			{toast && (
				<div
					className="fixed bottom-6 left-1/2 z-[60] -translate-x-1/2 rounded-xl border border-[#e5e7eb] bg-white px-5 py-3 text-body-sm font-medium text-[#0a0a0a] shadow-lg"
					role="status"
					aria-live="polite"
				>
					{toast}
				</div>
			)}
		</section>
	)
}
