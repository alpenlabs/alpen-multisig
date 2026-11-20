import { feeRateTenths } from '../model/payout-mapping'
import { useState } from 'react'
import * as api from '@/api/payout-admin'
import { unwrap } from './use-payout-admin'

export function useCreateBlockPayouts() {
	const [text, setText] = useState('')
	const [fee, setFee] = useState('1.0')
	const [reports, setReports] = useState<api.PayoutReports | null>(null)
	const [bundle, setBundle] = useState<api.PayoutBundle | null>(null)
	const [removed, setRemoved] = useState<string[]>([])
	const [confirmed, setConfirmed] = useState<string[]>([])
	const rate = feeRateTenths(fee)

	const rebundle = async (excluded = removed, verified = reports) => {
		setBundle(null)

		if (verified && rate !== null) setBundle(await unwrap(api.payoutBundle(verified.verificationId, rate, excluded)))
	}

	return {
		text,
		fee,
		reports,
		bundle,
		rate,

		setText: (value: string) => {
			setText(value)
			setReports(null)
			setBundle(null)
			setRemoved([])
			setConfirmed([])
		},

		setFee: (value: string) => {
			setFee(value)
			setBundle(null)
		},

		verify: async () => {
			setReports(null)
			setBundle(null)
			setRemoved([])
			setConfirmed([])
			const result = await unwrap(api.payoutVerifyReports(text))
			setReports(result)
			await rebundle([], result)
		},

		rebundle: () => rebundle(),

		remove: async (outpoint: string) => {
			const next = [...removed, outpoint]
			setRemoved(next)
			await rebundle(next)
		},

		confirmed: (transaction: api.BundledPayout) => {
			setConfirmed([...confirmed, transaction.transactionId])
			setRemoved([...removed, ...transaction.claims.map((claim) => claim.outpoint)])
		},

		transactions: bundle?.transactions.filter((tx) => !confirmed.includes(tx.transactionId)) ?? [],
	}
}
