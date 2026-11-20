import { Button } from '@/components/button'
import { useTransactionReview, type TransactionReviewProps } from '../hooks/use-transaction-review'
import { LedgerScreens, Waiting } from './ledger-screens'
import { Alert, KeyValue, Working } from './payout-ui'

export function SignBlockPayouts(props: TransactionReviewProps) {
	const { bundleId } = props
	const { review, signing, error, sign, retry } = useTransactionReview(props)
	// On the Block payouts page the figure rows sit right above, so the rows below continue them without a gap.
	const continuesRows = bundleId !== undefined && review !== null
	const errorAlert = error && <Alert kind="error">{error}</Alert>

	return (
		<div className={continuesRows ? undefined : 'mt-3'}>
			{!continuesRows && errorAlert}
			{review ? (
				<>
					{bundleId ? (
						<KeyValue label="Transaction ID" mono>
							{review.blockPayoutsTxid}
						</KeyValue>
					) : (
						<KeyValue label="Fee">
							{review.feeSat.toLocaleString('en-US')} sats · {review.vsize.toLocaleString('en-US')} vB
						</KeyValue>
					)}
					<KeyValue label={bundleId ? 'Change address' : 'Change'}>
						{!bundleId && `${review.changeSat.toLocaleString('en-US')} sats to `}
						<span className="font-mono text-label">{review.changeAddress}</span>
					</KeyValue>
					{continuesRows && errorAlert}
					{!bundleId && review.pinLockWarning && (
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
					{(signing || !props.signingTransaction) && <LedgerScreens screens={review.deviceScreens} />}
					{signing ? (
						<Waiting />
					) : (
						<Button size="sm" onClick={sign}>
							Sign on Ledger
						</Button>
					)}
				</>
			) : error ? (
				<Button size="sm" variant="secondary" onClick={retry}>
					Try again
				</Button>
			) : (
				<Working label="Checking the block payouts transaction…" />
			)}
		</div>
	)
}
