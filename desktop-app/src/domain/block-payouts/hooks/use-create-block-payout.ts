import { useState } from 'react'
import { payoutVerifyReports } from '@/api/payouts'
import type { ReportRow } from '../model/block-payouts.types'
export function useCreateBlockPayout() {
	const [step, setStep] = useState<'reports' | 'review'>('reports')
	const [rawReports, setRawReports] = useState('')
	const [reports, setReports] = useState<ReportRow[]>([])
	const [malformed, setMalformed] = useState<string[]>([])
	const [isBusy, setIsBusy] = useState(false)
	const [error, setError] = useState<string | null>(null)
	const ignoredCount = reports.filter((r) => r.verdict !== 'valid').length
	const handleVerify = async () => {
		setIsBusy(true)
		setError(null)
		const res = await payoutVerifyReports(rawReports)
		setIsBusy(false)
		if (!res.ok) {
			setError(res.error)
			return
		}
		const rows = res.data.reports
		setReports(rows)
		setMalformed(res.data.malformed.map((m) => `Item ${m.position + 1}: ${m.error}`))
		setStep('review')
	}
	return {
		step,
		rawReports,
		setRawReports,
		reports,
		malformed,
		isBusy,
		error,
		ignoredCount,
		handleVerify,
		handleBack: () => setStep('reports'),
	}
}
export type CreateBlockPayoutHook = ReturnType<typeof useCreateBlockPayout>
