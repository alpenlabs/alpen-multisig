import { feeRateTenths } from '../model/payout-mapping'
import { useEffect, useRef, useState } from 'react'
import * as api from '@/api/payout-admin'
import { unwrap } from './use-payout-admin'

const PREVIEW_DELAY_MS = 400

export function useCreateBlockPayouts() {
	const [text, setText] = useState('')
	const [fee, setFee] = useState('1.0')
	const [reports, setReports] = useState<api.PayoutReports | null>(null)
	const [bundle, setBundle] = useState<api.PayoutBundle | null>(null)
	const [removed, setRemoved] = useState<string[]>([])
	const [confirmed, setConfirmed] = useState<string[]>([])
	const [previewing, setPreviewing] = useState(false)
	const [previewError, setPreviewError] = useState<string | null>(null)
	// Only the answer to the latest request may become the preview.
	const latest = useRef(0)
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
	const rate = feeRateTenths(fee)

	useEffect(() => () => clearTimeout(timer.current), [])

	const rebundle = async (excluded = removed, verified = reports, feeRate = rate) => {
		latest.current += 1
		const request = latest.current
		clearTimeout(timer.current)
		setBundle(null)
		setPreviewError(null)

		if (!verified || feeRate === null) return

		const result = await unwrap(api.payoutBundle(verified.verificationId, feeRate, excluded))

		if (request === latest.current) setBundle(result)
	}

	// The fee preview follows the fee field by itself. It does not lock the screen, so typing is not interrupted.
	const preview = async (feeRate: number | null) => {
		setPreviewing(true)
		try {
			await rebundle(removed, reports, feeRate)
		} catch (error) {
			setPreviewError(error instanceof Error ? error.message : String(error))
		} finally {
			setPreviewing(false)
		}
	}

	return {
		text,
		fee,
		reports,
		bundle,
		rate,
		previewing,
		previewError,
		confirmedCount: confirmed.length,

		setText: (value: string) => {
			latest.current += 1
			clearTimeout(timer.current)
			setText(value)
			setReports(null)
			setBundle(null)
			setRemoved([])
			setConfirmed([])
			setPreviewError(null)
		},

		setFee: (value: string) => {
			latest.current += 1
			clearTimeout(timer.current)
			setFee(value)
			setBundle(null)
			setPreviewError(null)
			const next = feeRateTenths(value)

			if (reports && next !== null) timer.current = setTimeout(() => void preview(next), PREVIEW_DELAY_MS)
		},

		retryPreview: () => preview(rate),

		verify: async () => {
			setReports(null)
			setBundle(null)
			setRemoved([])
			setConfirmed([])
			const result = await unwrap(api.payoutVerifyReports(text))
			setReports(result)
			await rebundle([], result)
		},

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
