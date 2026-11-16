import { PayoutButton } from './payout-button'
import type { BlockPayoutsHook } from '../hooks/use-block-payouts'
import { useCreateBlockPayout } from '../hooks/use-create-block-payout'
import { amountUnit } from '../model/block-payouts.mapping'
import type { Tab } from '../model/block-payouts.types'
import { usePayoutAdminWallet } from '../hooks/use-payout-admin-wallet'
import { CreateBlockPayoutModal } from './create-block-payout-modal'
import { PayoutAdminWalletModal } from './payout-admin-wallet-modal'
import { PasteSignaturesModal } from './paste-signatures-modal'
import { PastTransactionsList } from './past-transactions-list'
import { PendingTransactionsList } from './pending-transactions-list'
import { SignBlockPayoutModal } from './sign-block-payout-modal'

type Props = { hook: BlockPayoutsHook }

const TAB_LABELS: { id: Tab; label: string }[] = [
	{ id: 'pending', label: 'Pending' },
	{ id: 'past', label: 'Past' },
]

function CreateFlow({ hook, unit }: Props & { unit: string }) {
	const flow = useCreateBlockPayout(hook.device)
	return <CreateBlockPayoutModal unit={unit} flow={flow} onCreated={hook.handleCreated} onClose={hook.closeModal} />
}

function AdminWalletFlow({ hook, unit }: Props & { unit: string }) {
	const adminWallet = usePayoutAdminWallet(hook.device)
	return <PayoutAdminWalletModal adminWallet={adminWallet} unit={unit} onClose={hook.closeModal} />
}

export function BlockPayoutsDashboard({ hook }: Props) {
	const { activeTab, pendingTxs, pastTxs, activeModal, toast, busyTxId, signIn } = hook
	const signTx = activeModal?.kind === 'sign' ? (pendingTxs.find((t) => t.txid === activeModal.txId) ?? null) : null
	const unit = signIn.phase === 'signed-in' ? amountUnit(signIn.network) : 'TEST'
	const pasteTxId = activeModal?.kind === 'paste' ? activeModal.txId : null

	return (
		<section className="mx-auto w-full max-w-200">
			<div className="mb-6 flex items-end justify-between gap-4">
				<div>
					<h1 className="m-0 font-display text-display-md font-normal leading-[1.2] tracking-[-0.005em] text-[#0a0a0a]">
						Block Payouts
					</h1>
					<p className="m-0 mt-1 text-body-sm leading-normal text-[#6b7280]">
						{signIn.phase === 'signed-in'
							? `Signed in as ${signIn.adminLabel} · ${signIn.threshold}-of-${signIn.adminCount} Payout Administrators · ${signIn.network} · Ledger: ${signIn.ledgerApp}`
							: 'Payout Administrator multisig.'}
					</p>
				</div>
				<div className="flex shrink-0 items-center gap-2">
					<PayoutButton appearance="toolbar" onClick={hook.openAdminWalletModal}>
						Admin Wallet
					</PayoutButton>
					<PayoutButton appearance="toolbar" onClick={hook.openImportModal}>
						Import transaction
					</PayoutButton>
					<PayoutButton appearance="create" onClick={hook.openCreateModal}>
						<span aria-hidden="true">+</span>
						Block payouts
					</PayoutButton>
				</div>
			</div>

			{hook.nodeWarning && (
				<div className="mb-4 rounded-xl border border-accent-border bg-highlight-surface px-4 py-3" role="status">
					<p className="m-0 text-body-sm text-emphasis">{hook.nodeWarning}</p>
				</div>
			)}

			<div className="mb-5 flex gap-0.5 rounded-xl border border-[#e5e7eb] bg-bg-base p-1">
				{TAB_LABELS.map(({ id, label }) => (
					<PayoutButton
						key={id}

						appearance="tab"
						style={
							activeTab === id
								? { background: '#fff', color: '#0a0a0a', boxShadow: '0 1px 3px rgba(0,0,0,0.08)' }
								: { background: 'transparent', color: '#6b7280' }
						}
						onClick={() => hook.setActiveTab(id)}
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
					</PayoutButton>
				))}
			</div>

			{activeTab === 'pending' ? (
				<PendingTransactionsList
					txs={pendingTxs}
					unit={unit}
					hasConflicts={hook.hasConflicts}
					busyTxId={busyTxId}
					onSign={hook.openSignModal}
					onPasteSignatures={hook.openPasteModal}
					onExport={hook.handleExport}
					onCopySignatures={(id) => void hook.handleCopySignatures(id)}
					onRetryBroadcast={(id) => void hook.handleRetryBroadcast(id)}
				/>
			) : (
				<PastTransactionsList
					txs={pastTxs}
					busyTxId={busyTxId}
					onRebroadcast={(id) => void hook.handleRebroadcast(id)}
					onCopyRawTx={hook.handleCopyRawTx}
				/>
			)}

			{signTx && (
				<SignBlockPayoutModal
					tx={signTx}
					unit={unit}
					isBusy={hook.isDeviceBusy}
					onSign={() => void hook.handleSign(signTx.txid)}
					onClose={hook.closeModal}
				/>
			)}

			{pasteTxId && (
				<PasteSignaturesModal
					title="Paste signatures"
					description="Paste signatures copied by other Payout Administrators, one per line. Valid signatures are added; invalid ones are listed."
					placeholder="txid:input:pubkey:signature — one per line"
					onSubmit={(raw) => hook.handlePasteSignatures(pasteTxId, raw)}
					onClose={hook.closeModal}
				/>
			)}

			{activeModal?.kind === 'import' && (
				<PasteSignaturesModal
					title="Import transaction"
					description="Paste a block payouts transaction exported from this app (with its signatures). It is checked against params.toml and added to Pending, or its signatures are merged."
					placeholder="PSBT (base64)"
					onSubmit={hook.handleImport}
					onClose={hook.closeModal}
				/>
			)}

			{activeModal?.kind === 'create' && <CreateFlow hook={hook} unit={unit} />}

			{activeModal?.kind === 'admin-wallet' && <AdminWalletFlow hook={hook} unit={unit} />}

			{toast && (
				<div
					className={`fixed bottom-6 left-1/2 z-50 max-w-150 -translate-x-1/2 rounded-xl border px-5 py-3 text-body-sm font-medium shadow-lg ${
						toast.kind === 'ok'
							? 'border-[#a7f3d0] bg-[#ecfdf5] text-[#065f46]'
							: 'border-danger-border bg-danger-surface text-danger-deep'
					}`}
					role="status"
					aria-live="polite"
				>
					{toast.message}
				</div>
			)}
		</section>
	)
}
