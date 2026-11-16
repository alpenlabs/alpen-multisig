import { truncate } from './modal-utils'
import { PayoutButton } from './payout-button'
import { PayoutCopyIcon } from './payout-icons'
import { PayoutClockIcon } from './payout-icons'
import { writeClipboard } from '@/api/tauri-bridge'
import { formatAmount, formatTimeRemaining } from '../model/block-payouts.mapping'
import type { PendingPayoutTx } from '../model/block-payouts.types'
import { ConflictingInputIcon } from './conflicting-input-icon'

type Props = {
	tx: PendingPayoutTx
	unit: string
	isBusy: boolean
	onSign: () => void
	onPasteSignatures: () => void
	onExport: () => void
	onCopySignatures: () => void
	onRetryBroadcast: () => void
}

export function PendingTransactionCard({
	tx,
	unit,
	isBusy,
	onSign,
	onPasteSignatures,
	onExport,
	onCopySignatures,
	onRetryBroadcast,
}: Props) {
	const { label: timeLabel, isUrgent: urgent } = formatTimeRemaining(tx.expiresAt)
	const progress = tx.signaturesNeeded === 0 ? 0 : Math.min((tx.signaturesReceived / tx.signaturesNeeded) * 100, 100)
	const hasQuorum = tx.signaturesReceived >= tx.signaturesNeeded

	function handleCopyId() {
		void writeClipboard(tx.txid)
	}

	return (
		<div className="rounded-xl border border-[#e5e7eb] bg-white px-5 py-4 shadow-sm">
			{/* Header row */}
			<div className="flex items-start justify-between gap-3">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<span className="font-mono text-body-sm font-medium text-[#0a0a0a]">{truncate(tx.txid, 12, 6)}</span>
						<PayoutButton appearance="copyId" onClick={handleCopyId} title="Copy transaction ID">
							<PayoutCopyIcon size={13} />
						</PayoutButton>
					</div>
					<p className="m-0 mt-0.5 text-label text-[#6b7280]">
						{tx.connectorCount} claim input{tx.connectorCount !== 1 ? 's' : ''} · fee {formatAmount(tx.feeSat, unit)} ·{' '}
						{tx.vsize.toLocaleString('en-US')} vB · created by {tx.createdBy}
					</p>
				</div>

				{/* Expiry badge */}
				<span
					className={`inline-flex shrink-0 items-center gap-1 rounded-md border border-[#e5e7eb] bg-[#f9fafb] px-2 py-0.5 text-mono-sm font-medium ${
						urgent ? 'text-emphasis' : 'text-[#6b7280]'
					}`}
				>
					<PayoutClockIcon size={11} />
					{timeLabel}
				</span>
			</div>

			{/* Signature progress */}
			<div className="mt-4">
				<div className="mb-1.5 flex items-center justify-between gap-2">
					<span className="text-body-sm font-medium text-[#0a0a0a]">Signatures</span>
					<span className="text-body-sm font-medium text-[#0a0a0a]">
						{tx.signaturesReceived} / {tx.signaturesNeeded}
					</span>
				</div>
				<div className="h-1.5 rounded-full bg-[#ebedf0]">
					<div
						className="h-1.5 rounded-full transition-all"
						style={{ width: `${progress}%`, background: hasQuorum ? '#0f9d7a' : '#6b7280' }}
					/>
				</div>
			</div>

			{tx.signers.length > 0 && (
				<p className="m-0 mt-1.5 text-label text-[#6b7280]">
					Signed by {tx.signers.map((signer) => signer.label).join(', ')}
				</p>
			)}

			{tx.broadcastError && (
				<div className="mt-3 rounded-lg border border-danger-border bg-danger-surface px-3 py-2">
					<p className="m-0 text-label font-medium text-danger-deep">Broadcast failed: {tx.broadcastError}</p>
					<PayoutButton disabled={isBusy} appearance="retry" onClick={onRetryBroadcast}>
						Retry broadcast
					</PayoutButton>
				</div>
			)}

			{/* Fully signed but still pending: its broadcast never happened (a lost response, a closed app). */}
			{tx.complete && !tx.broadcastError && (
				<div className="mt-3 rounded-lg border border-accent-border bg-highlight-surface px-3 py-2">
					<p className="m-0 text-label font-medium text-emphasis">Fully signed, but not broadcast yet.</p>
					<PayoutButton disabled={isBusy} appearance="broadcast" onClick={onRetryBroadcast}>
						Broadcast
					</PayoutButton>
				</div>
			)}

			{/* Inputs list */}
			<div className="mt-4">
				<p className="m-0 mb-1.5 text-label font-semibold uppercase tracking-wider text-[#9ca3af]">Inputs</p>
				<div className="flex flex-col gap-1">
					{tx.inputs.map((input) => (
						<div key={input.outpoint} className="flex items-center gap-1.5">
							<span className="font-mono text-label text-[#374151]" title={input.outpoint}>
								{truncate(input.outpoint, 16, 6)}
							</span>
							{input.duplicate && <ConflictingInputIcon />}
						</div>
					))}
				</div>
			</div>

			{/* Footer row */}
			<div className="mt-4 flex items-center justify-between gap-3 border-t border-[#f3f4f6] pt-3">
				{/* Signed status or Sign button */}
				{tx.signedByMe ? (
					<span className="inline-flex items-center gap-1.5 text-body-sm font-medium text-[#059669]">
						<svg width={14} height={14} viewBox="0 0 24 24" fill="none" aria-hidden>
							<path d="M5 12l5 5L20 7" stroke="#059669" strokeWidth="2" strokeLinecap="round" />
						</svg>
						Signed
					</span>
				) : (
					<PayoutButton disabled={isBusy} appearance="signCompact" onClick={onSign}>
						{isBusy ? 'Signing…' : 'Sign'}
					</PayoutButton>
				)}

				{/* Secondary actions */}
				<div className="flex items-center gap-1">
					<IconButton label="Paste signatures" onClick={onPasteSignatures}>
						<svg width={14} height={14} viewBox="0 0 24 24" fill="none" aria-hidden>
							<rect x="8" y="2" width="8" height="4" rx="1" stroke="currentColor" strokeWidth="1.5" />
							<path
								d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"
								stroke="currentColor"
								strokeWidth="1.5"
							/>
							<path d="M9 12h6M9 16h4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
						</svg>
					</IconButton>
					<IconButton label="Copy all signatures" onClick={onCopySignatures}>
						<PayoutCopyIcon size={14} />
					</IconButton>
					<IconButton label="Export transaction with signatures" onClick={onExport}>
						<svg width={14} height={14} viewBox="0 0 24 24" fill="none" aria-hidden>
							<path
								d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"
								stroke="currentColor"
								strokeWidth="1.5"
								strokeLinecap="round"
							/>
							<polyline
								points="7 10 12 15 17 10"
								stroke="currentColor"
								strokeWidth="1.5"
								strokeLinecap="round"
								strokeLinejoin="round"
							/>
							<line x1="12" y1="15" x2="12" y2="3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
						</svg>
					</IconButton>
				</div>
			</div>
		</div>
	)
}

function IconButton({ label, onClick, children }: { label: string; onClick: () => void; children: React.ReactNode }) {
	return (
		<PayoutButton title={label} aria-label={label} appearance="icon" onClick={onClick}>
			{children}
		</PayoutButton>
	)
}
