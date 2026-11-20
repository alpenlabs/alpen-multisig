import { useState } from 'react'
import { NodeConfigModal } from '@/domain/node-config/components/node-config-modal'
import { useNodeConfig } from '@/domain/node-config/hooks/use-node-config'
import { useWalletPanelData } from '@/domain/admin-wallet/hooks/use-wallet-panel-data'
import { WalletSessionControl } from '@/domain/admin-wallet/components/wallet-session-control'
import { useSessionAdminId } from '@/domain/admin-wallet/hooks/use-session-admin-id'
import { PayoutSignInPanel } from '@/domain/payout-admin/components/payout-sign-in-panel'
import { SignBlockPayoutsModal } from '@/domain/payout-admin/components/sign-block-payouts-modal'
import { useNavigate } from 'react-router-dom'
import { DisconnectButton } from '@/components/disconnect-button'
import { usePayoutAdmin } from '@/domain/payout-admin/hooks/use-payout-admin'
import { PayoutAdminDashboard } from '@/domain/payout-admin/components/payout-admin-dashboard'
import { ScreenShell } from '@/screens/screen-shell'
import { ShieldAccentIcon } from '@/assets/icons'
import { useSession } from '@/hooks/use-session'

export function PayoutAdminScreen() {
	const hook = usePayoutAdmin()
	const navigate = useNavigate()
	const { disconnectSession, wallet, sessionTimeLabel, sessionWarning } = useSession()
	const panel = useWalletPanelData()
	const adminId = useSessionAdminId(wallet)
	const node = useNodeConfig()
	const [settings, setSettings] = useState(false)

	async function handleDisconnect() {
		await disconnectSession()
		navigate('/')
	}

	return (
		<ScreenShell
			onOpenNodeSettings={() => {
				if (hook.busy) return

				hook.reset()
				setSettings(true)
			}}
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
					<button onClick={() => navigate('/')}>Back to multisig selection</button>
					<DisconnectButton onClick={() => void handleDisconnect()} />
				</fieldset>
			}
		>
			<fieldset disabled={hook.busy} className="contents">
				{hook.session ? (
					<PayoutAdminDashboard hook={hook} />
				) : (
					<PayoutSignInPanel
						hook={hook}
						config={node.config}
						onSettings={() => {
							hook.reset()
							setSettings(true)
						}}
					/>
				)}
				{hook.review && (
					<SignBlockPayoutsModal review={hook.review} onSign={hook.confirmReview} onClose={hook.closeReview} />
				)}
			</fieldset>
			{hook.busy && <p role="status">Working…</p>}
			<NodeConfigModal
				isOpen={settings}
				config={node.config}
				localNodeStatus={node.localNodeStatus}
				isSaving={node.isSaving}
				onRecheck={node.recheck}
				onSave={async (config) => {
					await node.saveConfig(config)
					setSettings(false)
				}}
				onClose={() => setSettings(false)}
			/>
		</ScreenShell>
	)
}
