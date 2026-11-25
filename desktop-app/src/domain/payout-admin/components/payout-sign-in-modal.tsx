import type { PayoutSignInReview } from '@/api/payout-admin'
import { LedgerScreens, Waiting } from './ledger-screens'
import { PayoutModal } from './payout-modal'
import { KeyValue } from './payout-ui'

type Props = {
	review: PayoutSignInReview
	onClose: () => void
}

export function PayoutSignInModal({ review, onClose }: Props) {
	return (
		<PayoutModal onClose={onClose}>
			<h2 className="m-0 mb-3 font-display text-display-sm font-normal text-[#0a0a0a]">Sign in on your Ledger</h2>
			<Waiting />
			<LedgerScreens screens={review.deviceScreens} />
			<KeyValue label="Coordinator" mono>
				{review.coordinatorUrl}
			</KeyValue>
			<KeyValue label="Admin key" mono>
				{review.adminKey}
			</KeyValue>
			<KeyValue label="Nonce expires">{new Date(review.expiresAtUnixSeconds * 1000).toLocaleString()}</KeyValue>
		</PayoutModal>
	)
}
