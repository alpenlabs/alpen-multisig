import { PayoutButton } from './payout-button'
import { truncate } from './modal-utils'
import { ReviewRow as Row } from './review-controls'
import { PayoutModal, PayoutHeading } from './payout-modal'
import { useEffect, useState } from 'react'
import { payoutReviewSign, type LedgerReview } from '@/api/payouts'
import { formatAmount } from '../model/block-payouts.mapping'
import type { PendingPayoutTx } from '../model/block-payouts.types'
import { LedgerChecklist } from './ledger-checklist'

type Props = {
	tx: PendingPayoutTx
	unit: string
	isBusy: boolean
	onSign: () => void
	onClose: () => void
}

export function SignBlockPayoutModal({ tx, unit, isBusy, onSign, onClose }: Props) {
	const [review, setReview] = useState<LedgerReview | null>(null)
	const [reviewError, setReviewError] = useState<string | null>(null)
	useEffect(() => {
		let live = true
		void payoutReviewSign(tx.txid).then((res) => {
			if (!live) return
			if (res.ok) setReview(res.data)
			else setReviewError(res.error)
		})
		return () => {
			live = false
		}
	}, [tx.txid])
	const remaining = Math.max(tx.signaturesNeeded - tx.signaturesReceived, 0)

	return (
		<PayoutModal onClose={isBusy ? () => undefined : onClose}>
			<div className="flex flex-col gap-5">
				<PayoutHeading title="Sign transaction">Review the transaction details before signing.</PayoutHeading>

				{/* Summary */}
				<div className="rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3.5">
					<dl className="flex flex-col gap-2">
						<Row
							label="Transaction ID"
							value={<span className="font-mono text-body-sm">{truncate(tx.txid, 12, 6)}</span>}
						/>
						<Row
							label="Ledger wallet name"
							value={<span className="font-mono text-body-sm">{tx.policyNames.join(', ')}</span>}
						/>
						<Row label="Claim inputs" value={`${tx.connectorCount} input${tx.connectorCount !== 1 ? 's' : ''}`} />
						<Row label="Fee (Admin Wallet)" value={`${formatAmount(tx.feeSat, unit)} · ${tx.vsize} vB`} />
						{tx.changeAddress && (
							<Row
								label={`Change to ${tx.createdBy}'s Admin Wallet`}
								value={
									<span className="flex flex-col gap-1">
										<span className="break-all font-mono text-label" title={tx.changeAddress}>
											{tx.changeAddress} ({formatAmount(tx.changeSat, unit)})
										</span>
										{tx.changeAddressOnDevice && (
											<span className="break-all text-label text-[#6b7280]">
												Regtest: your Ledger shows this as{' '}
												<span className="font-mono" title={tx.changeAddressOnDevice}>
													{tx.changeAddressOnDevice}
												</span>
											</span>
										)}
									</span>
								}
							/>
						)}
						<Row
							label="Signatures"
							value={
								<span>
									{tx.signaturesReceived} / {tx.signaturesNeeded}{' '}
									<span className="text-[#6b7280]">({remaining} more needed)</span>
								</span>
							}
						/>
					</dl>
				</div>

				<LedgerChecklist review={review} error={reviewError} unit={unit} />

				<p className="m-0 text-label text-[#6b7280]">
					Once {tx.signaturesNeeded} admins have signed, the transaction is broadcast.
				</p>

				{/* Actions */}
				<div className="flex items-center justify-end gap-2.5">
					<PayoutButton appearance="cancel" disabled={isBusy} onClick={onClose}>
						Cancel
					</PayoutButton>
					<PayoutButton disabled={isBusy || review === null} appearance="sign" onClick={onSign}>
						<svg width={13} height={13} viewBox="0 0 24 24" fill="none" aria-hidden>
							<path d="M12 20h9" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
							<path
								d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"
								stroke="currentColor"
								strokeWidth="1.5"
								strokeLinejoin="round"
							/>
						</svg>
						{isBusy ? 'Confirm on your Ledger…' : 'Sign'}
					</PayoutButton>
				</div>
			</div>
		</PayoutModal>
	)
}
