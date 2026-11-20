import type { PayoutReview, PayoutSignInReview } from '@/api/payout-admin'
import { Button } from '@/components/button'
import { PayoutModal } from './payout-modal'
import { Alert, KeyValue, Working } from './payout-ui'

/** The screens the Ledger shows, framed so the admin sees at once what to compare on the device. */
function LedgerScreens({ screens }: { screens: string[] }) {
	return (
		<div className="my-4 overflow-hidden rounded-xl border-2 border-[#0a0a0a]">
			<h3 className="m-0 bg-[#0a0a0a] px-3 py-1.5 text-label font-semibold uppercase tracking-wider text-white">
				Compare these screens on your Ledger
			</h3>
			<ol className="m-0 list-none p-0">
				{screens.map((screen, i) => (
					<li key={i} className="flex gap-3 border-t border-[#e5e7eb] px-3 py-2 first:border-t-0">
						<span className="flex size-5 shrink-0 items-center justify-center rounded bg-[#f3f4f6] text-label font-semibold text-[#374151]">
							{i + 1}
						</span>
						<span className="min-w-0 whitespace-pre-wrap font-mono text-body leading-6 wrap-anywhere text-[#0a0a0a]">
							{screen}
						</span>
					</li>
				))}
			</ol>
		</div>
	)
}

function Waiting() {
	return (
		<div>
			<Working label="Check each screen on your Ledger, then approve." />
			<p className="m-0 mt-1 text-body-sm text-[#6b7280]">
				Reject on the Ledger to cancel. Nothing changes here if you do.
			</p>
		</div>
	)
}

export function SignBlockPayoutsModal({
	review,
	signing,
	onSign,
	onClose,
}: {
	review: PayoutReview | PayoutSignInReview
	/** True while the Ledger shows its screens; the dialog then has nothing to click. */
	signing: boolean
	onSign: () => void
	onClose: () => void
}) {
	if (!('blockPayoutsTxid' in review)) {
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

	return (
		<PayoutModal onClose={onClose}>
			<h2 className="m-0 mb-3 font-display text-display-sm font-normal text-[#0a0a0a]">
				Sign block payouts transaction
			</h2>
			<KeyValue label="Transaction" mono>
				{review.blockPayoutsTxid}
			</KeyValue>
			<KeyValue label="Claim inputs">
				<details>
					<summary className="cursor-pointer">{review.claimInputCount}</summary>
					<ol className="m-0 mt-1 list-decimal pl-5 font-mono text-label leading-5">
						{review.claims.map((claim) => (
							<li key={claim.outpoint}>{claim.outpoint}</li>
						))}
					</ol>
				</details>
			</KeyValue>
			<KeyValue label="Fee">
				{review.feeSat.toLocaleString('en-US')} sats · {review.vsize.toLocaleString('en-US')} vB
			</KeyValue>
			<KeyValue label="Change">
				{review.changeSat.toLocaleString('en-US')} sats to{' '}
				<span className="font-mono text-label">{review.changeAddress}</span>
			</KeyValue>
			{review.pinLockWarning && (
				<Alert kind="warning">
					Ledger PIN lock may interrupt signing from 15 claims (about 2 seconds per claim). Unlock and retry if
					interrupted.
				</Alert>
			)}
			{review.warnings.map((warning) => (
				<Alert kind="warning" key={warning}>
					{warning}
				</Alert>
			))}
			<LedgerScreens screens={review.deviceScreens} />
			{signing ? (
				<Waiting />
			) : (
				<div className="flex justify-end gap-2">
					<Button size="sm" variant="secondary" onClick={onClose}>
						Cancel
					</Button>
					<Button size="sm" onClick={onSign}>
						Sign on Ledger
					</Button>
				</div>
			)}
		</PayoutModal>
	)
}
