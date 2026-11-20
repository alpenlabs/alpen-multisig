import type { PayoutReview, PayoutSignInReview } from '@/api/payout-admin'
import { PayoutModal } from './payout-modal'

export function SignBlockPayoutsModal({
	review,
	onSign,
	onClose,
}: {
	review: PayoutReview | PayoutSignInReview
	onSign: () => void
	onClose: () => void
}) {
	return (
		<PayoutModal onClose={onClose}>
			<h2 className="font-display text-display-sm">Review Payout Administrator action</h2>
			{'blockPayoutsTxid' in review ? (
				<div className="break-all">
					<p>Transaction: {review.blockPayoutsTxid}</p>
					<p>
						{review.claimInputCount} claim inputs · Fee: {review.feeSat} sats · {review.vsize} vB
					</p>
					{review.claims.map((claim) => (
						<p key={claim.outpoint}>{claim.outpoint}</p>
					))}
					<p>
						Change: {review.changeSat} sats to {review.changeAddress}
					</p>
					{review.pinLockWarning && (
						<p role="alert">
							Ledger PIN lock may interrupt signing from 15 claims (about 2 seconds per claim). Unlock and retry if
							interrupted.
						</p>
					)}
					{review.warnings.map((warning) => (
						<p key={warning} role="alert">
							{warning}
						</p>
					))}
				</div>
			) : (
				<div className="break-all">
					<p>Coordinator: {review.coordinatorUrl}</p>
					<p>Admin key: {review.adminKey}</p>
					<p>Nonce: {review.nonce}</p>
					<p>Expires: {new Date(review.expiresAtUnixSeconds * 1000).toLocaleString()}</p>
				</div>
			)}
			<h3>Compare these screens on your Ledger</h3>
			{review.deviceScreens.map((screen, i) => (
				<p key={i}>{screen}</p>
			))}
			<button onClick={onClose}>Cancel</button> <button onClick={onSign}>Continue on Ledger</button>
		</PayoutModal>
	)
}
