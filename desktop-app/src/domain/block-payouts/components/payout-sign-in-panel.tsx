import { PayoutError } from './payout-error'
import { PayoutButton } from './payout-button'
import type { PayoutSignIn } from '../model/block-payouts.types'
import { LedgerChecklist } from './ledger-checklist'

type Props = {
	signIn: PayoutSignIn
	onSignIn: () => void
	onConfirmSignIn: () => void
}

/** Shown until the connected Ledger is identified as a Payout Administrator and signed in. */
export function PayoutSignInPanel({ signIn, onSignIn, onConfirmSignIn }: Props) {
	const isLoading = signIn.phase === 'loading'
	const review = signIn.phase === 'review' || signIn.phase === 'signing' ? signIn.review : null
	return (
		<section className="mx-auto w-full max-w-130 rounded-2xl border border-[#e5e7eb] bg-white p-6 shadow-sm">
			<h1 className="m-0 font-display text-display-sm font-normal text-[#0a0a0a]">Payout Administrator sign-in</h1>
			<p className="m-0 mt-2 text-body-sm text-[#6b7280]">
				Connect your Ledger (Nano X or another Ledger with the Bitcoin app 2.4.6 or later open). The app reads the
				bridge parameters file and signer roster set in Node settings, checks that this Ledger is one of the Payout
				Administrators, and signs the coordinator&apos;s sign-in message on the device.
			</p>
			{signIn.phase === 'error' && <PayoutError spaced>{signIn.message}</PayoutError>}
			{review ? (
				<>
					<div className="mt-5">
						<LedgerChecklist review={review} error={null} unit="" />
					</div>
					<PayoutButton disabled={signIn.phase === 'signing'} appearance="connect" onClick={onConfirmSignIn}>
						{signIn.phase === 'signing' ? 'Confirm on your Ledger…' : 'Sign in on Ledger'}
					</PayoutButton>
				</>
			) : (
				<PayoutButton disabled={isLoading} appearance="connect" onClick={onSignIn}>
					{isLoading ? 'Connecting…' : 'Connect Ledger'}
				</PayoutButton>
			)}
		</section>
	)
}
