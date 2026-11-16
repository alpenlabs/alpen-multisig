import type { DeviceOperation } from '../model/device-operation'
import {
	feeRateError,
	payoutConfirm,
	payoutPlan,
	payoutReviewConfirm,
	payoutVerifyReports,
	type LedgerReview,
	type PayoutPlan,
	type PlanClaim,
	type PlanRequest,
} from '@/api/payouts'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { groupsWithout } from '../model/block-payouts.mapping'
import { createRequestGeneration, requestKey } from '../model/request-identity'
import type { ReportRow } from '../model/block-payouts.types'

export type CreateStep = 'reports' | 'review' | 'confirm'

export function useCreateBlockPayout(device: DeviceOperation) {
	const [step, setStep] = useState<CreateStep>('reports')
	const [rawReports, setRawReports] = useState('')
	const [reports, setReports] = useState<ReportRow[]>([])
	const [malformed, setMalformed] = useState<string[]>([])
	const [claims, setClaims] = useState<PlanClaim[]>([])
	const [groups, setGroups] = useState<string[][] | null>(null)
	const [feeRate, setFeeRateState] = useState('1')
	const [plan, setPlan] = useState<PayoutPlan | null>(null)
	const [review, setReview] = useState<LedgerReview | null>(null)
	const [reviewError, setReviewError] = useState<string | null>(null)
	const [reviewGeneration] = useState(createRequestGeneration)
	const [planGeneration] = useState(createRequestGeneration)
	const feeTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
	const [planRequest, setPlanRequest] = useState<PlanRequest | null>(null)
	const [reviewedKey, setReviewedKey] = useState<string | null>(null)
	const currentRequest = useMemo(() => ({ claims, feeRate, ...(groups ? { groups } : {}) }), [claims, feeRate, groups])
	const planMatches = planRequest !== null && requestKey(planRequest) === requestKey(currentRequest)
	const invalidateReview = useCallback(() => {
		reviewGeneration.next()
		setReviewedKey(null)
		setReview(null)
		setReviewError(null)
	}, [reviewGeneration])
	const [isBusy, setIsBusy] = useState(false)
	const [error, setError] = useState<string | null>(null)

	const feeRateMessage = useMemo(() => feeRateError(feeRate), [feeRate])
	const ignoredCount = reports.filter((r) => r.verdict !== 'valid').length

	const invalidatePlan = useCallback(() => {
		if (feeTimer.current) clearTimeout(feeTimer.current)
		planGeneration.next()
		setPlan(null)
		setPlanRequest(null)
	}, [planGeneration])

	const replan = useCallback(
		async (nextClaims: PlanClaim[], nextGroups: string[][] | null, rate: string, generation: number) => {
			if (!planGeneration.isCurrent(generation)) return
			if (nextClaims.length === 0 || feeRateError(rate) !== null) {
				setPlan(null)
				return
			}
			const request: PlanRequest = { claims: nextClaims, feeRate: rate, ...(nextGroups ? { groups: nextGroups } : {}) }
			const res = await payoutPlan(request)
			if (!planGeneration.isCurrent(generation)) return
			if (!res.ok) {
				setError(res.error)
				setPlan(null)
				return
			}
			setError(null)
			setPlan(res.data)
			setPlanRequest({ ...request, groups: res.data.txs.map((t) => t.claims.map((c) => c.outpoint)) })
			// Freeze the shape once planned so removing an input never reshuffles the others.
			setGroups(res.data.txs.map((t) => t.claims.map((c) => c.outpoint)))
		},
		[planGeneration],
	)

	const handleVerify = async () => {
		invalidateReview()
		invalidatePlan()
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
		const valid = rows
			.filter((r) => r.verdict === 'valid' && r.covenantIndex !== null)
			.map((r) => ({ outpoint: r.outpoint, covenantIndex: r.covenantIndex as number, payoutHeight: r.payoutHeight }))
		setClaims(valid)
		setGroups(null)
		setStep('review')
		await replan(valid, null, feeRate, planGeneration.snapshot())
	}

	// Re-plan shortly after typing stops, not on every keystroke.
	useEffect(
		() => () => {
			if (feeTimer.current) clearTimeout(feeTimer.current)
			reviewGeneration.next()
			planGeneration.next()
		},
		[planGeneration, reviewGeneration],
	)
	const setFeeRate = useCallback(
		(rate: string) => {
			invalidateReview()
			invalidatePlan()
			setFeeRateState(rate)
			if (feeTimer.current) clearTimeout(feeTimer.current)
			const generation = planGeneration.snapshot()
			feeTimer.current = setTimeout(() => void replan(claims, groups, rate, generation), 300)
		},
		[invalidateReview, invalidatePlan, planGeneration, replan, claims, groups],
	)

	const handleRemoveInput = (outpoint: string) => {
		invalidateReview()
		invalidatePlan()
		const nextClaims = claims.filter((c) => c.outpoint !== outpoint)
		const nextGroups = groups ? groupsWithout(groups, new Set([outpoint])) : null
		setClaims(nextClaims)
		setGroups(nextGroups)
		void replan(nextClaims, nextGroups, feeRate, planGeneration.snapshot())
	}

	const handleNext = async () => {
		if (!plan?.ok || !planMatches) return
		const generation = reviewGeneration.next()
		const request = planRequest!
		setStep('confirm')
		setReview(null)
		setReviewError(null)
		const res = await payoutReviewConfirm(request)
		if (!reviewGeneration.isCurrent(generation)) return
		if (res.ok) {
			setReviewedKey(requestKey(request))
			setReview(res.data)
		} else setReviewError(res.error)
	}

	const handleConfirm = async (): Promise<string | null> => {
		if (!plan?.ok || !planMatches || !review || reviewedKey !== requestKey(currentRequest)) return null
		const owner = device.begin()
		if (owner === null) return null
		setIsBusy(true)
		setError(null)
		let res
		try {
			res = await payoutConfirm(planRequest!, plan.totalFeeSat, review.txids)
		} finally {
			device.end(owner)
			setIsBusy(false)
		}
		if (!res.ok) {
			setError(res.error)
			return null
		}
		const failed = res.data.find((r) => r.broadcast && !r.broadcast.broadcast)
		if (failed) return `Broadcast failed: ${failed.broadcast?.error ?? 'unknown error'}. Use Retry broadcast.`
		const n = res.data.length
		return `${n} transaction${n === 1 ? '' : 's'} created and signed.`
	}

	return {
		step,
		rawReports,
		setRawReports,
		reports,
		ignoredCount,
		malformed,
		feeRate,
		setFeeRate,
		feeRateMessage,
		plan: planMatches ? plan : null,
		review: planMatches && reviewedKey === requestKey(currentRequest) ? review : null,
		reviewError,
		isBusy,
		error,
		handleVerify,
		handleRemoveInput,
		handleBack: () => {
			invalidateReview()
			if (step === 'review') invalidatePlan()
			setStep((s) => (s === 'confirm' ? 'review' : 'reports'))
		},
		handleNext: () => void handleNext(),
		handleConfirm,
	}
}

export type CreateBlockPayoutHook = ReturnType<typeof useCreateBlockPayout>
