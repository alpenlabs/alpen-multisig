import { useEffect, useRef, useState } from 'react'
import { NodeConfigModal } from '@/domain/node-config/components/node-config-modal'
import { useNodeConfig } from '@/domain/node-config/hooks/use-node-config'
import type { NodeConfig } from '@/domain/node-config/model/node-config.types'
import { useWalletPanelData } from '@/domain/admin-wallet/hooks/use-wallet-panel-data'
import { WalletSessionControl } from '@/domain/admin-wallet/components/wallet-session-control'
import { useSessionAdminId } from '@/domain/admin-wallet/hooks/use-session-admin-id'
import { useEnsureAdminWalletSession } from '@/domain/admin-wallet/hooks/use-ensure-admin-wallet-session'
import { PayoutSignInPanel } from '@/domain/payout-admin/components/payout-sign-in-panel'
import { SignBlockPayoutsModal } from '@/domain/payout-admin/components/sign-block-payouts-modal'
import { Working } from '@/domain/payout-admin/components/payout-ui'
import { useNavigate } from 'react-router-dom'
import { DisconnectButton } from '@/components/disconnect-button'
import { usePayoutAdmin } from '@/domain/payout-admin/hooks/use-payout-admin'
import { PayoutAdminDashboard } from '@/domain/payout-admin/components/payout-admin-dashboard'
import { ScreenShell } from '@/screens/screen-shell'
import { ShieldAccentIcon } from '@/assets/icons'
import { useSession } from '@/hooks/use-session'

/** True when saving `draft` would change nothing; an empty field equals a missing one. */
function sameSettings(saved: NodeConfig | null, draft: NodeConfig): boolean {
	if (saved === null) return false

	const keys = [...new Set([...Object.keys(saved), ...Object.keys(draft)])] as (keyof NodeConfig)[]

	return keys.every((key) => (saved[key] ?? '') === (draft[key] ?? ''))
}

export function PayoutAdminScreen() {
	const hook = usePayoutAdmin()
	const navigate = useNavigate()
	const { disconnectSession, wallet, adapter, sessionTimeLabel, sessionWarning } = useSession()
	// This route skips the role sign-in that starts the Admin Wallet elsewhere; block payouts pay fees from it.
	useEnsureAdminWalletSession(adapter)
	const panel = useWalletPanelData()
	const adminId = useSessionAdminId(wallet)
	const node = useNodeConfig()
	const [settings, setSettings] = useState(false)
	const { loadDeployment } = hook
	const initialLoad = useRef(false)

	// Saved payout admin files load by themselves when the screen opens; saving the settings loads them again.
	useEffect(() => {
		if (initialLoad.current || node.config === null) return

		initialLoad.current = true
		const { payoutParamsPath, payoutSignersPath } = node.config

		if (payoutParamsPath && payoutSignersPath) void loadDeployment(payoutParamsPath, payoutSignersPath)
	}, [node.config, loadDeployment])

	async function handleDisconnect() {
		await disconnectSession()
		navigate('/')
	}

	function openSettings() {
		if (!hook.busy) setSettings(true)
	}

	// Cancel changes nothing. Save signs the admin out only when a setting changed, then loads the files again.
	async function saveSettings(draft: NodeConfig) {
		const unchanged = sameSettings(node.config, draft)
		await node.saveConfig(draft)
		setSettings(false)

		if (unchanged) return

		hook.reset()

		if (draft.payoutParamsPath && draft.payoutSignersPath) {
			await hook.loadDeployment(draft.payoutParamsPath, draft.payoutSignersPath)
		}
	}

	return (
		<ScreenShell
			onOpenNodeSettings={openSettings}
			authorityBadge={
				<span className="inline-flex items-center gap-1.5 rounded-md border border-accent-border bg-bg-surface px-2.5 py-1 text-label font-medium text-accent-hover">
					<ShieldAccentIcon width={12} height={12} className="block shrink-0" />
					Payout Administrator
				</span>
			}
			headerContent={
				<fieldset disabled={hook.busy} className="contents">
					<WalletSessionControl
						hideAdminId
						panel={panel}
						adminId={adminId}
						sessionTimeLabel={sessionTimeLabel}
						sessionWarning={sessionWarning}
					/>
					<button
						type="button"
						className="inline-flex items-center rounded-lg border border-[#e5e7eb] bg-white px-2.5 py-1.25 text-label font-medium text-[#6b7280] transition hover:border-[#d1d5db] hover:bg-[#f9fafb] hover:text-emphasis"
						onClick={() => navigate('/')}
					>
						Back to multisig selection
					</button>
					<DisconnectButton onClick={() => void handleDisconnect()} />
				</fieldset>
			}
		>
			<fieldset disabled={hook.busy} className="contents">
				{hook.session ? (
					<PayoutAdminDashboard hook={hook} />
				) : (
					<PayoutSignInPanel hook={hook} config={node.config} status={node.localNodeStatus} onSettings={openSettings} />
				)}
				{hook.review && (
					<SignBlockPayoutsModal
						review={hook.review}
						signing={hook.signing}
						onSign={hook.confirmReview}
						onClose={hook.closeReview}
					/>
				)}
			</fieldset>
			{hook.working && !hook.review && (
				<div className="fixed bottom-6 left-1/2 z-[60] -translate-x-1/2 rounded-xl border border-[#e5e7eb] bg-white px-5 py-3 shadow-lg">
					<Working label={hook.working} />
				</div>
			)}
			<NodeConfigModal
				isOpen={settings}
				config={node.config}
				localNodeStatus={node.localNodeStatus}
				isSaving={node.isSaving}
				onRecheck={node.recheck}
				onSave={saveSettings}
				onClose={() => setSettings(false)}
			/>
		</ScreenShell>
	)
}
