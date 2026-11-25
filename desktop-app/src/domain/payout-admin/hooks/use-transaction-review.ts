import { useEffect, useState } from 'react'
import { payoutReviewPending, payoutReviewTransaction, type PayoutReview } from '@/api/payout-admin'
import { SIGNING_TRANSACTION, type PayoutAdminHook } from './use-payout-admin'

export type TransactionSigningProps = Pick<PayoutAdminHook, 'handleError' | 'run' | 'signingTransaction'> & {
	onSign: (reviewId: string, includesRegistration: boolean) => Promise<void>
}

export type TransactionReviewProps = TransactionSigningProps & {
	transactionId: string
	bundleId?: string
	revision?: number
}

export function useTransactionReview({
	transactionId,
	bundleId,
	revision,
	handleError,
	run,
	onSign,
}: TransactionReviewProps) {
	const [review, setReview] = useState<PayoutReview | null>(null)
	const [error, setError] = useState<string | null>(null)
	const [attempt, setAttempt] = useState(0)
	const [signing, setSigning] = useState(false)

	useEffect(() => {
		let current = true
		setReview(null)
		const request =
			bundleId === undefined ? payoutReviewPending(transactionId) : payoutReviewTransaction(bundleId, transactionId)
		void request.then((result) => {
			if (!current) return

			if (result.ok) setReview(result.data)
			else handleError(result.error, (message) => setError((previous) => previous ?? message))
		})

		return () => {
			current = false
		}
	}, [transactionId, bundleId, revision, attempt, handleError])

	function sign() {
		if (!review) return

		void run(
			async () => {
				setError(null)
				setSigning(true)
				try {
					await onSign(review.reviewId, review.deviceScreens.includes('Register account'))

					if (bundleId === undefined) setAttempt((previous) => previous + 1)
				} finally {
					setSigning(false)
					setReview(null)
				}
			},
			SIGNING_TRANSACTION,
			setError,
		)
	}

	function retry() {
		setError(null)
		setAttempt(attempt + 1)
	}

	return { review, signing, error, sign, retry }
}
