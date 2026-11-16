import { useNavigate } from 'react-router-dom'
import { DisconnectButton } from '@/components/disconnect-button'
import { useBlockPayouts } from '@/domain/block-payouts/hooks/use-block-payouts'
import { BlockPayoutsDashboard } from '@/domain/block-payouts/components/block-payouts-dashboard'
import { PayoutSignInPanel } from '@/domain/block-payouts/components/payout-sign-in-panel'
import { ScreenShell } from '@/screens/screen-shell'
import { ShieldAccentIcon } from '@/assets/icons'
import { useSession } from '@/hooks/use-session'

export function BlockPayoutsScreen() {
	const hook = useBlockPayouts()
	const navigate = useNavigate()
	const { disconnectSession } = useSession()

	// PRD §3.d: close this multisig and go back to choosing one, with the Ledger still connected.
	async function handleBackToMultisigs() {
		if (hook.device.isBusy()) return
		await hook.handleSignOut()
		navigate('/', { state: { authorityStep: 'select-authority' } })
	}

	async function handleDisconnect() {
		if (hook.device.isBusy()) return
		await hook.handleSignOut()
		await disconnectSession()
		navigate('/')
	}

	return (
		<ScreenShell
			authorityBadge={
				<span className="inline-flex items-center gap-1.5 rounded-md border border-accent-border bg-bg-surface px-2.5 py-1 text-label font-medium text-accent-hover">
					<ShieldAccentIcon width={12} height={12} className="block shrink-0" />
					Payout Administrator
				</span>
			}
			headerContent={
				<>
					<button
						type="button"
						className="inline-flex items-center gap-1.5 rounded-lg border border-[#e5e7eb] bg-white px-2.5 py-1.25 text-label font-medium text-[#6b7280] transition hover:border-[#d1d5db] hover:bg-[#f9fafb] hover:text-emphasis"
						onClick={() => void handleBackToMultisigs()}
					>
						Back to multisigs
					</button>
					<DisconnectButton onClick={() => void handleDisconnect()} />
				</>
			}
		>
			{hook.signIn.phase === 'signed-in' ? (
				<BlockPayoutsDashboard hook={hook} />
			) : (
				<PayoutSignInPanel
					signIn={hook.signIn}
					onSignIn={() => void hook.handleSignIn()}
					onConfirmSignIn={() => void hook.handleConfirmSignIn()}
				/>
			)}
		</ScreenShell>
	)
}
