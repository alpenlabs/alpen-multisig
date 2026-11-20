import { mapPending } from '../model/payout-mapping'
import { useState, useEffect, useRef, useCallback } from 'react'
import * as api from '@/api/payout-admin'
import { writeClipboard } from '@/api/tauri-bridge'
import type { ApiResult } from '@/types'
import type { PastBlockPayoutsTx, ActiveModal } from '../model/payout-admin.types'

export type Tab = 'pending' | 'past'

export type PasteResult = { ok: true } | { ok: false; invalidSignatures: string[] }

export type PayoutAdminHook = ReturnType<typeof usePayoutAdmin>

export function usePayoutAdmin() {
	const [activeTab, setActiveTab] = useState<Tab>('pending')
	const [overview, setOverview] = useState<api.PayoutOverview>({ pending: [], past: [], hidden: 0 })
	const [stale, setStale] = useState(false)
	const [session, setSession] = useState<api.PayoutSession | null>(null)
	const [identity, setIdentity] = useState<api.PayoutIdentity | null>(null)
	const [deployment, setDeployment] = useState<api.PayoutDeployment | null>(null)
	const [activeModal, setActiveModal] = useState<ActiveModal>(null)
	const [review, setReview] = useState<api.PayoutReview | api.PayoutSignInReview | null>(null)
	const [confirm, setConfirm] = useState<(() => Promise<void>) | null>(null)
	const [toast, setToast] = useState<string | null>(null)
	const [busy, setBusy] = useState(false)
	const lock = useRef(false)

	const clearSession = useCallback(() => {
		setSession(null)
		setReview(null)
		setConfirm(null)
		setActiveModal(null)
		setOverview({ pending: [], past: [], hidden: 0 })
		setStale(false)
	}, [])

	const handleError = useCallback(
		(message: string) => {
			if (/^(Coordinator 401\b|Sign in again: session expired)/.test(message)) clearSession()

			setToast(message)
		},
		[clearSession],
	)

	const run = useCallback(
		async (action: () => Promise<void>) => {
			if (lock.current) return

			lock.current = true
			setBusy(true)
			setToast(null)
			try {
				await action()
			} catch (error) {
				handleError(String(error instanceof Error ? error.message : error))
			} finally {
				lock.current = false
				setBusy(false)
			}
		},
		[handleError],
	)

	const refresh = useCallback(async () => {
		const result = await api.payoutOverview()

		if (!result.ok) {
			setStale(true)
			throw new Error(result.error)
		}

		setOverview(result.data)
		setStale(false)
	}, [])

	useEffect(() => {
		if (!session) return

		let live = true
		let polling = false

		const poll = async () => {
			if (session.expiresAtUnixSeconds <= Date.now() / 1000) {
				clearSession()

				return
			}

			if (polling) return

			polling = true
			const result = await api.payoutOverview()
			polling = false

			if (live) {
				setStale(!result.ok)
				if (result.ok) setOverview(result.data)
				else handleError(result.error)
			}
		}

		void poll()
		const timer = setInterval(() => void poll(), 15_000)

		return () => {
			live = false
			clearInterval(timer)
		}
	}, [session, clearSession, handleError])

	const closeModal = () => {
		if (!lock.current) {
			setActiveModal(null)
			setReview(null)
			setConfirm(null)
		}
	}

	const prepare = async (
		request: Promise<ApiResult<api.PayoutReview | api.PayoutSignInReview>>,
		action: (id: string) => Promise<void>,
	) => {
		const value = await unwrap(request)
		setReview(value)
		setConfirm(() => async () => {
			setReview(null)
			setConfirm(null)
			await action(value.reviewId)
		})
	}

	const showPaste = (value: api.PayoutPasteResult): PasteResult =>
		value.invalid.length
			? { ok: false, invalidSignatures: value.invalid.map((invalid) => `${invalid.raw}: ${invalid.reason}`) }
			: { ok: true }

	const pendingTxs = overview.pending.map((tx) => mapPending(tx, session?.adminKey ?? ''))

	const pastTxs: PastBlockPayoutsTx[] = overview.past.map((tx) => ({
		id: tx.txid,
		status: tx.status,
		blockTimestamp:
			tx.status === 'confirmed' && tx.blockTimeUnixSeconds !== null
				? new Date(tx.blockTimeUnixSeconds * 1000)
				: undefined,
		rawTx: tx.rawTx,
	}))

	return {
		stale,
		hidden: overview.hidden,
		activeTab,
		setActiveTab,
		pendingTxs,
		pastTxs,
		session,
		identity,
		deployment,
		review,
		busy,
		run,
		toast,
		activeModal,
		hasConflicts: pendingTxs.some((tx) => tx.inputs.some((input) => input.isConflicting)),
		closeModal,
		closeReview: () => {
			if (lock.current) return

			setReview(null)
			setConfirm(null)
		},

		reset: () => {
			clearSession()
			setDeployment(null)
			setIdentity(null)
		},

		readIdentity: () =>
			run(async () => {
				setIdentity(null)
				setIdentity(await unwrap(api.payoutIdentity()))
			}),

		showKey: () =>
			run(async () => {
				setIdentity(await unwrap(api.payoutShowKey()))
			}),

		loadDeployment: (params: string, signers: string) =>
			run(async () => {
				setDeployment(null)
				try {
					setDeployment(await unwrap(api.payoutLoadDeployment(params, signers)))
				} catch (error) {
					throw new Error(`Deployment / signer file refused: ${String(error)}`)
				}
			}),

		signIn: (url: string) =>
			run(() =>
				prepare(api.payoutSignInReview(url), async (id) => {
					setSession(await unwrap(api.payoutSignIn(id)))
				}),
			),

		confirmReview: () =>
			run(async () => {
				if (confirm) await confirm()
			}),

		openSignModal: (txid: string) =>
			run(() =>
				prepare(api.payoutReviewPending(txid), async (id) => {
					const result = showPaste(await unwrap(api.payoutSignPending(id)))
					await refresh()

					if (!result.ok) setToast(result.invalidSignatures.join('\n'))
				}),
			),

		openPasteModal: (txId: string) => setActiveModal({ kind: 'paste', txId }),
		openCreateModal: () => setActiveModal({ kind: 'create' }),

		handlePasteSignatures: async (txid: string, text: string): Promise<PasteResult> => {
			let result: PasteResult = { ok: false, invalidSignatures: ['Import failed; see error message.'] }
			await run(async () => {
				result = showPaste(await unwrap(api.payoutPasteSignatures(txid, text)))
				await refresh()
			})

			return result
		},

		handleImport: (text: string) =>
			run(async () => {
				await unwrap(api.payoutImportPsbt(text))
				await refresh()
			}),

		handleExport: (id: string) =>
			run(async () => {
				await writeClipboard(await unwrap(api.payoutExportPsbt(id)))
				setToast('Base64 PSBT with signatures copied.')
			}),

		handleCopySignatures: (id: string) =>
			run(async () => {
				await writeClipboard(await unwrap(api.payoutCopySignatures(id)))
				setToast('Signatures copied.')
			}),

		handleRebroadcast: (id: string) =>
			run(async () => {
				const result = await unwrap(api.payoutRebroadcast(id))

				if (result.broadcastError) throw new Error(result.broadcastError)

				await refresh()
			}),

		handleRetry: (id: string) =>
			run(async () => {
				const result = await unwrap(api.payoutRetryBroadcast(id))
				await refresh()

				if (result.broadcastError) throw new Error(result.broadcastError)
			}),

		handleCopyRawTx: (id: string) =>
			run(async () => {
				const tx = pastTxs.find((tx) => tx.id === id)

				if (tx) {
					await writeClipboard(tx.rawTx)
					setToast('Transaction with signatures copied.')
				}
			}),

		reviewTransaction: (bundleId: string, id: string, onConfirmed: () => void) =>
			run(() =>
				prepare(api.payoutReviewTransaction(bundleId, id), async (reviewId) => {
					await unwrap(api.payoutConfirmTransaction(reviewId))
					onConfirmed()
					await refresh()
				}),
			),
	}
}

export async function unwrap<T>(request: Promise<ApiResult<T>>): Promise<T> {
	const result = await request

	if (!result.ok) throw new Error(result.error)

	return result.data
}
