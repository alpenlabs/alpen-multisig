import { canStartDeviceOperation, ownsDeviceOperation } from '../model/device-operation'
import { createRequestGeneration } from '../model/request-identity'
import { writeClipboard } from '@/api/tauri-bridge'
import { getOrchestratorBaseUrl } from '@/api/orchestrator-auth'
import {
	payoutImport,
	payoutLoadDeployment,
	payoutOverview,
	payoutPasteSignatures,
	payoutRebroadcast,
	payoutRetryBroadcast,
	payoutSign,
	payoutSignaturesText,
	payoutSignIn,
	payoutSignInReview,
	payoutSignOut,
	type BroadcastResult,
} from '@/api/payouts'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ActiveModal, PastPayoutTx, PayoutSignIn, PendingPayoutTx, Tab, Toast } from '../model/block-payouts.types'

const POLL_MS = 15_000

/** Outcome of Paste signatures, as the modal shows it. */
export type PasteOutcome = { ok: true } | { ok: false; message: string }

function broadcastMessage(b: BroadcastResult | null, signed: string): Toast {
	if (b === null) return { kind: 'ok', message: signed }
	if (b.broadcast) return { kind: 'ok', message: 'Quorum reached — transaction broadcast to the Bitcoin network.' }
	return { kind: 'error', message: `Broadcast failed: ${b.error ?? 'unknown error'}. Use Retry broadcast.` }
}

function downloadText(text: string, filename: string): void {
	const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }))
	const a = document.createElement('a')
	a.href = url
	a.download = filename
	document.body.appendChild(a)
	a.click()
	document.body.removeChild(a)
	URL.revokeObjectURL(url)
}

export function useBlockPayouts() {
	const [signIn, setSignIn] = useState<PayoutSignIn>({ phase: 'idle' })
	const [activeTab, setActiveTab] = useState<Tab>('pending')
	const [pendingTxs, setPendingTxs] = useState<PendingPayoutTx[]>([])
	const [pastTxs, setPastTxs] = useState<PastPayoutTx[]>([])
	const [nodeWarning, setNodeWarning] = useState<string | null>(null)
	const [activeModal, setActiveModal] = useState<ActiveModal>(null)
	const [toast, setToast] = useState<Toast | null>(null)
	const [busyTxId, setBusyTxId] = useState<string | null>(null)
	const deviceOwner = useRef<symbol | null>(null)
	const [isDeviceBusy, setIsDeviceBusy] = useState(false)
	const [overviewGeneration] = useState(createRequestGeneration)
	const overviewActive = useRef(false)
	const overviewPending = useRef<number | null>(null)
	const mutations = useRef(0)
	const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

	const isSignedIn = signIn.phase === 'signed-in'
	const hasConflicts = useMemo(() => pendingTxs.some((tx) => tx.hasDuplicates), [pendingTxs])

	const showToast = useCallback((t: Toast) => {
		setToast(t)
		if (toastTimer.current) clearTimeout(toastTimer.current)
		toastTimer.current = setTimeout(() => setToast(null), t.kind === 'error' ? 8000 : 3500)
	}, [])

	useEffect(
		() => () => {
			if (toastTimer.current) clearTimeout(toastTimer.current)
		},
		[],
	)

	const invalidateOverview = useCallback(() => {
		overviewGeneration.next()
		overviewPending.current = null
	}, [overviewGeneration])

	const beginDevice = useCallback(() => {
		if (!canStartDeviceOperation(deviceOwner.current)) return null
		const owner = Symbol('Ledger operation')
		deviceOwner.current = owner
		setIsDeviceBusy(true)
		mutations.current++
		invalidateOverview()
		return owner
	}, [invalidateOverview])
	const endDevice = useCallback(
		(owner: symbol) => {
			if (!ownsDeviceOperation(deviceOwner.current, owner)) return
			deviceOwner.current = null
			setIsDeviceBusy(false)
			mutations.current--
			invalidateOverview()
		},
		[invalidateOverview],
	)
	const deviceBusy = useCallback(() => !canStartDeviceOperation(deviceOwner.current), [])
	const device = useMemo(
		() => ({ begin: beginDevice, end: endDevice, isBusy: deviceBusy }),
		[beginDevice, endDevice, deviceBusy],
	)
	const changeModal = useCallback(
		(modal: ActiveModal) => {
			if (!deviceBusy()) setActiveModal(modal)
		},
		[deviceBusy],
	)

	const mutate = useCallback(
		async <T>(action: () => Promise<T>): Promise<T> => {
			mutations.current++
			invalidateOverview()
			try {
				return await action()
			} finally {
				mutations.current--
				invalidateOverview()
			}
		},
		[invalidateOverview],
	)

	const refresh = useCallback(async () => {
		if (!overviewActive.current || mutations.current > 0) return
		const generation = overviewGeneration.next()
		overviewPending.current = generation
		const res = await payoutOverview()
		if (!overviewGeneration.isCurrent(generation)) return
		overviewPending.current = null
		if (!res.ok) {
			setNodeWarning(res.error)
			return
		}
		setPendingTxs(res.data.pending)
		setPastTxs(res.data.past)
		setNodeWarning(res.data.nodeWarning)
	}, [overviewGeneration])

	useEffect(() => {
		if (!isSignedIn) return
		overviewActive.current = true
		void refresh()
		const id = setInterval(() => {
			if (overviewPending.current === null) void refresh()
		}, POLL_MS)
		return () => {
			clearInterval(id)
			overviewActive.current = false
			invalidateOverview()
		}
	}, [isSignedIn, refresh, invalidateOverview])

	const handleSignIn = async () => {
		const owner = device.begin()
		if (owner === null) return
		try {
			setSignIn({ phase: 'loading' })
			const dep = await payoutLoadDeployment()
			if (!dep.ok) {
				setSignIn({ phase: 'error', message: dep.error })
				return
			}
			const review = await payoutSignInReview(getOrchestratorBaseUrl())
			if (!review.ok) {
				setSignIn({ phase: 'error', message: review.error })
				return
			}
			setSignIn({ phase: 'review', review: review.data })
		} finally {
			device.end(owner)
		}
	}

	const handleConfirmSignIn = async () => {
		const owner = device.begin()
		if (owner === null) return
		try {
			if (signIn.phase !== 'review') return
			setSignIn({ phase: 'signing', review: signIn.review })
			const dep = await payoutLoadDeployment()
			if (!dep.ok) {
				setSignIn({ phase: 'error', message: dep.error })
				return
			}
			const session = await payoutSignIn(getOrchestratorBaseUrl())
			if (!session.ok) {
				setSignIn({ phase: 'error', message: session.error })
				return
			}
			setSignIn({
				phase: 'signed-in',
				adminLabel: session.data.admin.label,
				threshold: dep.data.threshold,
				adminCount: dep.data.admins.length,
				network: dep.data.network,
				ledgerApp: session.data.ledgerApp,
			})
		} finally {
			device.end(owner)
		}
	}

	const handleSignOut = async () => {
		if (device.isBusy()) return
		overviewActive.current = false
		invalidateOverview()
		await payoutSignOut()
		setSignIn({ phase: 'idle' })
		setPendingTxs([])
		setPastTxs([])
	}

	const handleSign = async (txId: string) => {
		const owner = device.begin()
		if (owner === null) return
		setBusyTxId(txId)
		try {
			const res = await mutate(() => payoutSign(txId))
			setBusyTxId(null)
			setActiveModal(null)
			if (!res.ok) {
				showToast({ kind: 'error', message: res.error })
			} else {
				showToast(broadcastMessage(res.data.broadcast, 'Signed. Waiting for more signatures.'))
			}
		} finally {
			device.end(owner)
			setBusyTxId(null)
		}
		await refresh()
	}

	const handlePasteSignatures = async (txId: string, raw: string): Promise<PasteOutcome> => {
		const res = await mutate(() => payoutPasteSignatures(txId, raw))
		if (!res.ok) return { ok: false, message: res.error }
		await refresh()
		if (res.data.added > 0 || res.data.alreadyPresent > 0) {
			showToast(
				broadcastMessage(res.data.broadcast, `${res.data.added} signature${res.data.added === 1 ? '' : 's'} added.`),
			)
		}
		return res.data.message ? { ok: false, message: res.data.message } : { ok: true }
	}

	const handleImport = async (raw: string): Promise<PasteOutcome> => {
		const res = await mutate(() => payoutImport(raw))
		if (!res.ok) return { ok: false, message: res.error }
		await refresh()
		showToast(broadcastMessage(res.data.broadcast, 'Transaction imported.'))
		return { ok: true }
	}

	const handleExport = (txId: string) => {
		const tx = pendingTxs.find((t) => t.txid === txId)
		if (tx) downloadText(tx.psbt, `block-payout-${txId.slice(0, 8)}.psbt.txt`)
	}

	const handleCopySignatures = async (txId: string) => {
		const res = await payoutSignaturesText(txId)
		if (!res.ok) {
			showToast({ kind: 'error', message: res.error })
			return
		}
		await writeClipboard(res.data)
		showToast({ kind: 'ok', message: 'Signatures copied to clipboard.' })
	}

	const handleRetryBroadcast = async (txId: string) => {
		if (device.isBusy()) return
		setBusyTxId(txId)
		const res = await mutate(() => payoutRetryBroadcast(txId))
		setBusyTxId(null)
		showToast(res.ok ? broadcastMessage(res.data, '') : { kind: 'error', message: res.error })
		await refresh()
	}

	const handleRebroadcast = async (txId: string) => {
		if (device.isBusy()) return
		setBusyTxId(txId)
		const res = await mutate(() => payoutRebroadcast(txId))
		setBusyTxId(null)
		showToast(res.ok ? { kind: 'ok', message: 'Transaction rebroadcast.' } : { kind: 'error', message: res.error })
	}

	const handleCopyRawTx = (txId: string) => {
		const tx = pastTxs.find((t) => t.txid === txId)
		if (!tx) return
		void writeClipboard(tx.rawTx)
		showToast({ kind: 'ok', message: 'Signed transaction copied to clipboard.' })
	}

	const handleCreated = (message: string) => {
		setActiveModal(null)
		setActiveTab('pending')
		showToast({ kind: message.startsWith('Broadcast failed') ? 'error' : 'ok', message })
		void refresh()
	}

	return {
		device,
		isDeviceBusy,
		signIn,
		activeTab,
		pendingTxs,
		pastTxs,
		hasConflicts,
		nodeWarning,
		activeModal,
		toast,
		busyTxId,
		setActiveTab,
		handleSignIn,
		handleConfirmSignIn,
		handleSignOut,
		refresh,
		openSignModal: (txId: string) => changeModal({ kind: 'sign', txId }),
		openPasteModal: (txId: string) => changeModal({ kind: 'paste', txId }),
		openImportModal: () => changeModal({ kind: 'import' }),
		openAdminWalletModal: () => changeModal({ kind: 'admin-wallet' }),
		openCreateModal: () => changeModal({ kind: 'create' }),
		closeModal: () => changeModal(null),
		handleSign,
		handlePasteSignatures,
		handleImport,
		handleExport,
		handleCopySignatures,
		handleRetryBroadcast,
		handleRebroadcast,
		handleCopyRawTx,
		handleCreated,
	}
}

export type BlockPayoutsHook = ReturnType<typeof useBlockPayouts>
