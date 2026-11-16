import {
	feeRateError,
	payoutPlan,
	payoutVerifyReports,
	type PayoutPlan,
	type PlanClaim,
	type PlanRequest,
} from '@/api/payouts'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { groupsWithout } from '../model/block-payouts.mapping'
import { createRequestGeneration, requestKey } from '../model/request-identity'
import type { ReportRow } from '../model/block-payouts.types'

export type CreateStep = 'reports' | 'review'

export function useCreateBlockPayout() {
	const [step, setStep] = useState<CreateStep>('reports')
	const [rawReports, setRawReports] = useState('')
	const [reports, setReports] = useState<ReportRow[]>([])
	const [malformed, setMalformed] = useState<string[]>([])
	const [claims, setClaims] = useState<PlanClaim[]>([])
	const [groups, setGroups] = useState<string[][] | null>(null)
	const [feeRate, setFeeRateState] = useState('1')
	const [plan, setPlan] = useState<PayoutPlan | null>(null)
	const [reviewGeneration] = useState(createRequestGeneration)
	const [planGeneration] = useState(createRequestGeneration)
	const feeTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
	const [planRequest, setPlanRequest] = useState<PlanRequest | null>(null)
	const currentRequest = useMemo(() => ({ claims, feeRate, ...(groups ? { groups } : {}) }), [claims, feeRate, groups])
	const planMatches = planRequest !== null && requestKey(planRequest) === requestKey(currentRequest)
	const invalidateReview = useCallback(() => {
		reviewGeneration.next()
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
		isBusy,
		error,
		handleVerify,
		handleRemoveInput,
		handleBack: () => {
			invalidateReview()
			if (step === 'review') invalidatePlan()
			setStep('reports')
		},
	}
}

export type CreateBlockPayoutHook = ReturnType<typeof useCreateBlockPayout>
