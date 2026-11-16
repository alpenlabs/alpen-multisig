import { useCallback, useEffect, useRef, useState } from 'react'
import {
	payoutWalletFeeRate,
	payoutWalletPrepare,
	payoutWalletExecute,
	type PayoutWalletSendReview,
} from '@/api/payouts'
import type { DeviceOperation } from '../model/device-operation'
import { createRequestGeneration } from '../model/request-identity'
import {
	canConfirmWallet,
	keepsReviewAfterFailure,
	walletCriticalError,
	walletFormKey,
	walletSendInput,
	type WalletForm,
} from '../model/wallet-send'

const emptyForm: WalletForm = { address: '', amount: '', max: false, feeRate: '', bumpTxid: null }

export function usePayoutWalletSend(device: DeviceOperation, refresh: () => Promise<void>, isLoading: boolean) {
	const [form, setForm] = useState(emptyForm)
	const [review, setReview] = useState<PayoutWalletSendReview | null>(null)
	const [validatedKey, setValidatedKey] = useState<string | null>(null)
	const [error, setError] = useState<string | null>(null)
	// Kept apart from `error`: the form is validated again after a failed send, and that must not erase why it failed.
	const [failure, setFailure] = useState<string | null>(null)
	const [txid, setTxid] = useState<string | null>(null)
	const [busy, setBusy] = useState(false)
	const [noEstimate, setNoEstimate] = useState(false)
	const [maxAmount, setMaxAmount] = useState<string>('')
	const [generation] = useState(createRequestGeneration)
	const working = useRef(false)
	const feeEdited = useRef(false)
	const currentForm = useRef(form)

	const handleEdit = useCallback(
		(patch: Partial<WalletForm>) => {
			generation.next()
			if (patch.feeRate !== undefined) feeEdited.current = true
			currentForm.current = { ...currentForm.current, ...patch }
			setForm(currentForm.current)
			setReview(null)
			setMaxAmount('')
			setValidatedKey(null)
			setError(null)
			setFailure(null)
		},
		[generation],
	)

	const invalidate = useCallback(() => {
		generation.next()
	}, [generation])

	useEffect(() => {
		let active = true
		const started = generation.snapshot()
		void payoutWalletFeeRate().then((res) => {
			if (!active) return
			if (!res.ok) {
				if (generation.isCurrent(started)) setError(res.error)
				return
			}
			setNoEstimate(res.data === null)
			if (!feeEdited.current && res.data !== null) handleEdit({ feeRate: res.data })
		})
		return () => {
			active = false
			invalidate()
		}
	}, [generation, handleEdit, invalidate])

	// Review is also the contract's validation endpoint: check all fields before enabling Confirm.
	const prepare = useCallback(
		async (showReview: boolean) => {
			const input = walletSendInput(currentForm.current)
			if (!input || working.current || isLoading) return
			const owner = device.begin()
			if (owner === null) return
			working.current = true
			setBusy(true)
			const started = generation.snapshot()
			const snapshot = currentForm.current
			try {
				const res = await payoutWalletPrepare(input)
				if (!generation.isCurrent(started)) return
				setError(res.ok ? null : walletCriticalError(res.error))
				setValidatedKey(res.ok ? walletFormKey(snapshot) : null)
				if (res.ok && snapshot.max) setMaxAmount((res.data.amountSat / 100_000_000).toFixed(8))
				if (showReview) setReview(res.ok ? res.data : null)
				return true
			} finally {
				working.current = false
				device.end(owner)
				setBusy(false)
			}
		},
		[device, generation, isLoading],
	)

	useEffect(() => {
		if (review || validatedKey === walletFormKey(form) || !walletSendInput(form)) return
		let active = true
		const attempt = async () => {
			const completed = await prepare(false)
			if (active && !completed) timer = window.setTimeout(() => void attempt(), 400)
		}
		let timer = window.setTimeout(() => void attempt(), 400)
		return () => {
			active = false
			window.clearTimeout(timer)
		}
	}, [form, prepare, review, validatedKey])

	const handleSign = async () => {
		if (!review || !canConfirmWallet(form, validatedKey, busy || isLoading) || working.current) return
		const input = walletSendInput(form)
		if (!input) return
		const owner = device.begin()
		if (owner === null) return
		working.current = true
		setBusy(true)
		setError(null)
		setFailure(null)
		const started = generation.snapshot()
		let succeeded = false
		let failed = false
		try {
			const res = await payoutWalletExecute(input, review.txid)
			if (!generation.isCurrent(started)) return
			if (!res.ok) {
				if (keepsReviewAfterFailure(res.error)) return
				// The transaction changed, a coin was spent, or the broadcast failed: what was reviewed no longer
				// describes the wallet, so it must be reviewed again.
				setFailure(res.error)
				setReview(null)
				setValidatedKey(null)
				failed = true
				return
			}
			setTxid(res.data.txid)
			handleEdit({ ...emptyForm, feeRate: form.feeRate })
			succeeded = true
		} finally {
			working.current = false
			device.end(owner)
			setBusy(false)
		}
		if (succeeded || failed) await refresh()
	}

	return {
		form,
		review,
		error: failure ?? error,
		txid,
		busy,
		noEstimate,
		maxAmount,
		canConfirm: canConfirmWallet(form, validatedKey, busy || isLoading),
		handleEdit,
		handleReview: () => prepare(true),
		handleSign,
		handleBack: () => handleEdit({ bumpTxid: null }),
	}
}

export type PayoutWalletSendHook = ReturnType<typeof usePayoutWalletSend>
