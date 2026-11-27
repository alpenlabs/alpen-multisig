import { mapPending } from '../model/payout-mapping'
import { useState, useEffect, useRef, useCallback } from 'react'
import * as api from '@/api/payout-admin'
import { writeClipboard } from '@/api/tauri-bridge'
import type { ApiResult } from '@/types'
import type { PastBlockPayoutsTx, ActiveModal } from '../model/payout-admin.types'

export const SIGNING_TRANSACTION = 'Signing the block payouts transaction…'

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
	// Why the payout admin files were refused; stays until a load succeeds or the admin signs out.
	const [deploymentError, setDeploymentError] = useState<string | null>(null)
	const [activeModal, setActiveModal] = useState<ActiveModal>(null)
	const [review, setReview] = useState<api.PayoutSignInReview | null>(null)
	const [toast, setToast] = useState<string | null>(null)
	const [error, setError] = useState<string | null>(null)
	// What the app is doing right now, shown next to a spinner; null when idle.
	const [working, setWorking] = useState<string | null>(null)
	const lock = useRef(false)
	const signingTransaction = working === SIGNING_TRANSACTION

	const clearSession = useCallback(() => {
		setSession(null)
		setReview(null)
		setActiveModal(null)
		setOverview({ pending: [], past: [], hidden: 0 })
		setStale(false)
	}, [])

	const handleError = useCallback(
		(message: string, report: (message: string) => void = setError) => {
			if (/^(Coordinator 401\b|Sign in again: session expired)/.test(message)) {
				clearSession()
				setError(message)
			} else report(message)
		},
		[clearSession],
	)

	const run = useCallback(
		async (action: () => Promise<void>, label = 'Working…', report?: (message: string) => void) => {
			if (lock.current) return

			lock.current = true
			setWorking(label)
			setToast(null)
			setError(null)
			try {
				await action()
			} catch (error) {
				handleError(String(error instanceof Error ? error.message : error), report)
			} finally {
				lock.current = false
				setWorking(null)
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
		// Keep the reviewed row in place until the Ledger returns.
		if (!session || signingTransaction) return

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
	}, [session, signingTransaction, clearSession, handleError])

	useEffect(() => {
		if (!toast) return

		const timer = setTimeout(() => setToast(null), 4000)

		return () => clearTimeout(timer)
	}, [toast])

	const closeModal = () => {
		if (!lock.current) {
			setActiveModal(null)
			setReview(null)
		}
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
		deploymentError,
		review,
		handleError,
		busy: working !== null,
		working,
		signingTransaction,
		run,
		toast,
		error,
		dismissError: () => setError(null),
		toSign: pendingTxs.filter((tx) => tx.verified && !tx.signedByCurrentUser).length,
		activeModal,
		hasConflicts: pendingTxs.some((tx) => tx.inputs.some((input) => input.isConflicting)),
		closeModal,
		// Signs out and forgets the loaded payout admin files. The Ledger identity stays: no setting changes it.
		reset: () => {
			void api.payoutSignOut()
			clearSession()
			setDeployment(null)
			setDeploymentError(null)
		},

		readIdentity: () =>
			run(async () => {
				setIdentity(null)
				setIdentity(await unwrap(api.payoutIdentity()))
			}, 'Reading the Ledger identity…'),

		showKey: () =>
			run(async () => {
				setIdentity(await unwrap(api.payoutShowKey()))
			}, 'Check the address on your Ledger…'),

		loadDeployment: (signers: string) =>
			run(async () => {
				setDeployment(null)
				setDeploymentError(null)
				const result = await api.payoutLoadDeployment(signers)

				if (result.ok) setDeployment(result.data)
				else setDeploymentError(result.error)
			}, 'Loading ASM history and signers.toml…'),

		// Starts the Ledger at once: the sign-in review has nothing to decide on this screen, only text to compare.
		signIn: (url: string) =>
			run(async () => {
				if (!identity) setIdentity(await unwrap(api.payoutIdentity()))

				const value = await unwrap(api.payoutSignInReview(url))
				setReview(value)
				try {
					setSession(await unwrap(api.payoutSignIn(value.reviewId)))
				} finally {
					setReview(null)
				}
			}, 'Waiting for your Ledger…'),

		signPending: async (reviewId: string) => {
			const result = showPaste(await unwrap(api.payoutSignPending(reviewId)))
			await refresh()

			if (!result.ok) throw new Error(result.invalidSignatures.join('\n'))

			setToast('Signature added.')
		},

		openVerifyModal: (txId: string) => {
			setError(null)
			setActiveModal({ kind: 'verify', txId })
		},

		handleVerify: async (txId: string, text: string): Promise<boolean> => {
			let verified = false
			await run(async () => {
				await unwrap(api.payoutVerifyPendingWithReport(txId, text))
				await refresh()
				verified = true
			}, 'Verifying the report…')

			return verified
		},

		openPasteModal: (txId: string) => {
			setError(null)
			setActiveModal({ kind: 'paste', txId })
		},

		openCreateModal: () => {
			setError(null)
			setActiveModal({ kind: 'create' })
		},

		openImportModal: () => {
			setError(null)
			setActiveModal({ kind: 'import' })
		},

		finishCreate: (created: number) => {
			setActiveModal(null)
			// A transaction that already has enough signatures is broadcast at once and is listed under Past.
			setActiveTab(overview.pending.length === 0 ? 'past' : 'pending')
			setToast(
				created === 1
					? 'The block payouts transaction was created with your signature.'
					: `${created} block payouts transactions were created with your signature.`,
			)
		},

		handlePasteSignatures: async (txid: string, text: string): Promise<PasteResult> => {
			let result: PasteResult = { ok: false, invalidSignatures: ['Import failed; see error message.'] }
			await run(async () => {
				result = showPaste(await unwrap(api.payoutPasteSignatures(txid, text)))
				await refresh()
			})

			return result
		},

		handleImport: async (text: string): Promise<boolean> => {
			let imported = false
			await run(async () => {
				await unwrap(api.payoutImportPsbt(text))
				imported = true
				setToast('PSBT imported.')
				await refresh()
			}, 'Importing the PSBT…')

			return imported
		},

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

		confirmTransaction: async (reviewId: string, onConfirmed: () => void) => {
			await unwrap(api.payoutConfirmTransaction(reviewId))
			onConfirmed()
			await refresh().catch((error: Error) => handleError(error.message))
		},
	}
}

export async function unwrap<T>(request: Promise<ApiResult<T>>): Promise<T> {
	const result = await request

	if (!result.ok) throw new Error(result.error)

	return result.data
}
