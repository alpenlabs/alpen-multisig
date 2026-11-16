import { usePayoutWalletSend } from './use-payout-wallet-send'
import { shouldReloadWallet } from '../model/wallet-send'
import { payoutWalletTip } from '@/api/payouts'
import type { DeviceOperation } from '../model/device-operation'
import { matchesReceiveAddress } from '../model/wallet-verification'
import { createRequestGeneration } from '../model/request-identity'
import { useCallback, useEffect, useRef, useState } from 'react'
import { payoutAdminWallet, payoutShowReceiveAddress, type PayoutAdminWallet } from '@/api/payouts'

export type DeviceCheck =
	{ status: 'idle' } | { status: 'busy' } | { status: 'ok'; address: string } | { status: 'error'; message: string }

/** The Admin Wallet the payout fees come from: coins, balance and the next receive address. */
export function usePayoutAdminWallet(device: DeviceOperation) {
	const [wallet, setWallet] = useState<PayoutAdminWallet | null>(null)
	const [error, setError] = useState<string | null>(null)
	const [stale, setStale] = useState<string | null>(null)
	const [isLoading, setIsLoading] = useState(true)
	const [deviceCheck, setDeviceCheck] = useState<DeviceCheck>({ status: 'idle' })

	const [generation] = useState(createRequestGeneration)
	const checking = useRef(false)
	const loading = useRef(false)
	const active = useRef(false)
	const currentWallet = useRef<PayoutAdminWallet | null>(null)
	const attemptedTip = useRef<number | null>(null)

	const handleRefresh = useCallback(
		async function refresh() {
			if (checking.current || loading.current || device.isBusy()) return
			loading.current = true
			const started = generation.next()
			setDeviceCheck({ status: 'idle' })
			setIsLoading(true)
			const res = await payoutAdminWallet()
			loading.current = false
			if (!generation.isCurrent(started)) {
				if (active.current) void refresh()
				return
			}
			setIsLoading(false)
			if (!res.ok) {
				setError(res.error)
				return
			}
			setError(null)
			currentWallet.current = res.data
			setWallet(res.data)
			setDeviceCheck({ status: 'idle' })
		},
		[device, generation],
	)

	const invalidate = useCallback(() => {
		active.current = false
		generation.next()
	}, [generation])
	useEffect(() => {
		active.current = true
		void handleRefresh()
		return invalidate
	}, [handleRefresh, invalidate])

	const handleShowOnLedger = useCallback(async () => {
		if (!wallet || loading.current || checking.current) return
		const owner = device.begin()
		if (owner === null) return
		checking.current = true
		const started = generation.snapshot()
		setDeviceCheck({ status: 'busy' })
		let res
		try {
			res = await payoutShowReceiveAddress(wallet.receiveIndex)
		} finally {
			checking.current = false
			device.end(owner)
		}
		if (!generation.isCurrent(started) || !currentWallet.current) return
		if (res.ok && !matchesReceiveAddress(wallet, currentWallet.current, res.data)) {
			setDeviceCheck({ status: 'error', message: 'The Ledger address does not match the current receive address.' })
			return
		}
		setDeviceCheck(res.ok ? { status: 'ok', address: res.data } : { status: 'error', message: res.error })
	}, [wallet, device, generation])

	useEffect(() => {
		let active = true
		let polling = false
		const timer = window.setInterval(async () => {
			if (polling || loading.current || device.isBusy()) return
			polling = true
			const started = generation.snapshot()
			try {
				const res = await payoutWalletTip()
				if (!active || !generation.isCurrent(started)) return
				setStale(res.ok ? null : `Bitcoin Core could not be asked (${res.error}). What is shown may be out of date.`)
				if (
					res.ok &&
					shouldReloadWallet(
						res.data,
						currentWallet.current?.tipHeight,
						attemptedTip.current,
						loading.current,
						device.isBusy(),
					)
				) {
					attemptedTip.current = res.data
					await handleRefresh()
				}
			} finally {
				polling = false
			}
		}, 15_000)
		return () => {
			active = false
			window.clearInterval(timer)
		}
	}, [device, generation, handleRefresh])

	const send = usePayoutWalletSend(device, handleRefresh, isLoading)
	return { send, wallet, error, stale, isLoading, deviceCheck, handleRefresh, handleShowOnLedger }
}

export type PayoutAdminWalletHook = ReturnType<typeof usePayoutAdminWallet>
