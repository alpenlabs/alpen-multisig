import { Button } from '@/components/button'
import { CopyButton } from '@/components/copy-button'
import type { PendingBlockPayoutsTx } from '../model/payout-admin.types'
import { Alert, KeyValue } from './payout-ui'

type Props = {
	tx: PendingBlockPayoutsTx
	open: boolean
	onToggle: () => void
	onSign: () => void
	onPasteSignatures: () => void
	onExport: () => void
	onCopySignatures: () => void
	onRetry: () => void
}

/** Column widths shared by the header and every row of the pending list. */
export const PENDING_COLUMNS = 'grid grid-cols-[5rem_minmax(0,1fr)_4.5rem_5.5rem_9.5rem] items-center gap-x-4'

function formatTimeRemaining(expiresAt: Date): { label: string; urgent: boolean } {
	const ms = expiresAt.getTime() - Date.now()
	if (ms <= 0) return { label: 'Expired', urgent: true }
	const totalMinutes = Math.floor(ms / 60000)
	const hours = Math.floor(totalMinutes / 60)
	const minutes = totalMinutes % 60
	const urgent = hours < 24
	if (hours >= 24) {
		const days = Math.floor(hours / 24)
		const remHours = hours % 24
		return { label: `${days}d ${remHours}h`, urgent: false }
	}
	return { label: `${hours}h ${minutes}m`, urgent }
}

function truncate(str: string, head = 10, tail = 8): string {
	if (str.length <= head + tail + 3) return str
	return `${str.slice(0, head)}…${str.slice(-tail)}`
}

export function PendingTransactionRow({
	tx,
	open,
	onToggle,
	onSign,
	onPasteSignatures,
	onExport,
	onCopySignatures,
	onRetry,
}: Props) {
	const { label: timeLabel, urgent } = formatTimeRemaining(tx.expiresAt)
	const shared = tx.inputs.some((input) => input.isConflicting)

	return (
		<div className="border-t border-[#f3f4f6] first:border-t-0">
			<div className={`${PENDING_COLUMNS} px-5 py-2.5 text-body-sm text-[#0a0a0a]`}>
				<span className={urgent ? 'font-semibold text-emphasis' : ''} title={tx.expiresAt.toLocaleString()}>
					{timeLabel}
				</span>
				<span className="flex min-w-0 items-center gap-1">
					<span title={tx.id} className="truncate font-mono font-medium">
						{truncate(tx.id, 12, 6)}
					</span>
					<CopyButton text={tx.id} variant="icon" />
				</span>
				<span title={shared ? 'A claim input is also in another pending transaction.' : undefined}>
					{tx.claimInputCount}
					{shared && ' ⚠'}
				</span>
				<span className={tx.signaturesRequired <= tx.adminsSigned ? 'font-medium text-[#059669]' : ''}>
					{tx.adminsSigned} / {tx.signaturesRequired}
				</span>
				<span className="flex items-center justify-end gap-2">
					{tx.signedByCurrentUser ? (
						<span className="whitespace-nowrap font-medium text-[#059669]">Signed ✓</span>
					) : (
						<Button size="sm" onClick={onSign}>
							Sign
						</Button>
					)}
					<Button size="sm" variant="ghost" aria-expanded={open} onClick={onToggle}>
						{open ? 'Hide' : 'Details'}
					</Button>
				</span>
			</div>

			{tx.broadcastError && (
				<div className="px-5 pb-2">
					<Alert kind="error">
						<p className="m-0">Not broadcast: {tx.broadcastError}</p>
						<Button size="sm" variant="secondary" className="mt-2" onClick={onRetry}>
							Retry broadcast
						</Button>
					</Alert>
				</div>
			)}

			{open && (
				<div className="mx-5 mb-3 rounded-lg border border-[#e5e7eb] border-t-2 border-t-[#0a0a0a] bg-bg-base px-4 py-3">
					<KeyValue label="Transaction ID" mono>
						{tx.id}
					</KeyValue>
					<KeyValue label="Expires">{tx.expiresAt.toLocaleString()}</KeyValue>
					<p className="m-0 mb-1 mt-3 text-body-sm font-semibold text-[#0a0a0a]">Claim inputs</p>
					<ol className="m-0 list-decimal pl-6 font-mono text-label leading-5 text-[#374151]">
						{tx.inputs.map((input) => (
							<li key={input.outpoint} className="py-0.5">
								<span className="wrap-anywhere">{input.outpoint}</span>
								{input.isConflicting && (
									<span className="block font-sans text-emphasis">⚠ Also in another pending transaction</span>
								)}
							</li>
						))}
					</ol>
					<div className="mt-3 flex flex-wrap gap-2">
						<Button size="sm" variant="secondary" onClick={onCopySignatures}>
							Copy signatures
						</Button>
						<Button size="sm" variant="secondary" onClick={onPasteSignatures}>
							Paste signatures
						</Button>
						<Button size="sm" variant="secondary" onClick={onExport}>
							Copy PSBT (base64)
						</Button>
					</div>
				</div>
			)}
		</div>
	)
}
