import { Button } from '@/components/button'
import type { PayoutAdminHook, Tab } from '../hooks/use-payout-admin'
import { PendingTransactionsList } from './pending-transactions-list'
import { PastTransactionsList } from './past-transactions-list'
import { PasteSignaturesModal } from './paste-signatures-modal'
import { CreateBlockPayouts } from './create-block-payouts'
import { ImportPsbtModal } from './import-psbt-modal'
import { Alert } from './payout-ui'

export function PayoutAdminDashboard({ hook }: { hook: PayoutAdminHook }) {
	const {
		activeTab,
		pendingTxs,
		pastTxs,
		hasConflicts,
		activeModal,
		toast,
		toSign,
		setActiveTab: onTabChange,
		openPasteModal,
		openCreateModal,
		openImportModal,
		closeModal,
		handlePasteSignatures,
		handleExport,
		handleCopySignatures,
		handleRebroadcast,
		handleCopyRawTx,
	} = hook
	const pasteTxId = activeModal?.kind === 'paste' ? activeModal.txId : null
	const tabs: { id: Tab; label: string }[] = [
		{ id: 'pending', label: 0 < toSign ? `Pending (${toSign} to sign)` : 'Pending' },
		{ id: 'past', label: 'Past' },
	]

	const toastBox = toast && (
		<div
			className="fixed bottom-6 left-1/2 z-[60] -translate-x-1/2 rounded-xl border border-[#e5e7eb] bg-white px-5 py-3 text-body-sm font-medium text-[#0a0a0a] shadow-lg"
			role="status"
			aria-live="polite"
		>
			{toast}
		</div>
	)

	if (activeModal?.kind === 'create') {
		return (
			<>
				<CreateBlockPayouts hook={hook} />
				{toastBox}
			</>
		)
	}

	return (
		<section className="mx-auto w-full max-w-200">
			{/* Page header */}
			<div className="mb-5 flex items-end justify-between gap-4">
				<h1 className="m-0 font-display text-display-md font-normal leading-[1.2] tracking-[-0.005em] text-[#0a0a0a]">
					Payout Admin
				</h1>
				<div className="flex shrink-0 items-center gap-2">
					<Button size="sm" variant="secondary" onClick={openImportModal}>
						Import PSBT
					</Button>
					<Button size="sm" onClick={openCreateModal}>
						<span aria-hidden="true">+</span>
						Block payouts
					</Button>
				</div>
			</div>

			{hook.error && activeModal === null && (
				<Alert kind="error" onDismiss={hook.dismissError}>
					{hook.error}
				</Alert>
			)}
			{hook.stale && <Alert kind="warning">Pending and Past numbers are stale. The coordinator refresh failed.</Alert>}

			{/* Tabs */}
			<div className="mb-4 flex gap-1 border-b border-[#e5e7eb]" role="tablist">
				{tabs.map(({ id, label }) => (
					<button
						key={id}
						type="button"
						role="tab"
						aria-selected={activeTab === id}
						className={`-mb-px border-b-2 px-4 py-2 text-body-sm transition ${
							activeTab === id
								? 'border-[#0a0a0a] font-semibold text-[#0a0a0a]'
								: 'border-transparent font-medium text-[#6b7280] hover:text-[#0a0a0a]'
						}`}
						onClick={() => onTabChange(id)}
					>
						{label}
					</button>
				))}
			</div>

			{activeTab === 'pending' && 0 < hook.hidden && (
				<Alert kind="warning">
					{hook.hidden} pending block payouts transaction(s) do not match your payout configuration and are hidden.
				</Alert>
			)}
			{/* Tab content */}
			{activeTab === 'pending' ? (
				<PendingTransactionsList
					txs={pendingTxs}
					hasConflicts={hasConflicts}
					signingProps={{
						handleError: hook.handleError,
						run: hook.run,
						signingTransaction: hook.signingTransaction,
						onSign: hook.signPending,
					}}
					onVerify={hook.openVerifyModal}
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
				<PasteSignaturesModal
					txId={pasteTxId}
					error={hook.error}
					onSubmit={handlePasteSignatures}
					onClose={closeModal}
				/>
			)}

			{activeModal?.kind === 'verify' && (
				<ImportPsbtModal
					verifyReport
					error={hook.error}
					onImport={(text) => hook.handleVerify(activeModal.txId, text)}
					onClose={closeModal}
				/>
			)}

			{activeModal?.kind === 'import' && (
				<ImportPsbtModal error={hook.error} onImport={hook.handleImport} onClose={closeModal} />
			)}

			{toastBox}
		</section>
	)
}
