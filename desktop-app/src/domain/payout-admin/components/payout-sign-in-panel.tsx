import type { ReactNode } from 'react'
import { Button } from '@/components/button'
import { getOrchestratorBaseUrl } from '@/api/orchestrator-auth'
import type { LocalNodeStatus, NodeConfig } from '@/domain/node-config/model/node-config.types'
import type { PayoutAdminHook } from '../hooks/use-payout-admin'
import { Alert, KeyValue } from './payout-ui'

function Step({
	number,
	title,
	done = false,
	disabled = false,
	actions,
	children,
}: {
	number: number
	title: string
	done?: boolean
	disabled?: boolean
	actions: ReactNode
	children: ReactNode
}) {
	return (
		<li className="flex gap-3 border-t border-[#f3f4f6] py-4 first:border-t-0">
			<span
				className={`flex size-7 shrink-0 items-center justify-center rounded-full border text-body-sm font-semibold ${
					done ? 'border-[#059669] bg-[#059669] text-white' : 'border-[#0a0a0a] text-[#0a0a0a]'
				} ${disabled ? 'opacity-40' : ''}`}
				aria-hidden="true"
			>
				{done ? '✓' : number}
			</span>
			<div className="min-w-0 flex-1">
				<div className="flex flex-wrap items-center justify-between gap-2">
					<h2 className={`m-0 text-body font-semibold text-[#0a0a0a] ${disabled ? 'opacity-40' : ''}`}>{title}</h2>
					<div className="flex items-center gap-2">{actions}</div>
				</div>
				<div className="mt-1 text-body-sm text-[#6b7280]">{children}</div>
			</div>
		</li>
	)
}

export function PayoutSignInPanel({
	hook,
	config,
	status,
	onSettings,
}: {
	hook: PayoutAdminHook
	config: NodeConfig | null
	status: LocalNodeStatus | null
	onSettings: () => void
}) {
	const { identity, deployment } = hook
	// The one dark button is the next thing to do. Step 1 may be skipped: step 3 reads the identity itself.
	const next = !identity ? 1 : !deployment ? 2 : 3

	return (
		<section className="mx-auto w-full max-w-200 rounded-xl border border-[#e5e7eb] bg-white p-6">
			<h1 className="m-0 font-display text-display-md font-normal text-[#0a0a0a]">Sign in as Payout Administrator</h1>
			{hook.error && (
				<Alert kind="error" onDismiss={hook.dismissError}>
					{hook.error}
				</Alert>
			)}
			<ol className="m-0 mt-2 list-none p-0">
				<Step
					number={1}
					title="Ledger identity"
					done={identity !== null}
					actions={
						identity ? (
							<>
								<Button size="sm" variant="secondary" onClick={hook.showKey}>
									Show on Ledger
								</Button>
								<Button size="sm" variant="ghost" onClick={hook.readIdentity}>
									Read again
								</Button>
							</>
						) : (
							<Button size="sm" variant={next === 1 ? 'primary' : 'secondary'} onClick={hook.readIdentity}>
								Read Ledger identity
							</Button>
						)
					}
				>
					{identity ? (
						<div className="text-[#0a0a0a]">
							<KeyValue label="Fingerprint" mono>
								{identity.fingerprint}
							</KeyValue>
							<KeyValue label="Admin key" mono>
								{identity.adminKey}
							</KeyValue>
							<KeyValue label="Account xpub" mono>
								{identity.accountXpub}
							</KeyValue>
							<KeyValue label="Expected address" mono>
								{identity.expectedAddress}
							</KeyValue>
							<p className="m-0 mt-1 text-[#6b7280]">
								"Show on Ledger" displays the Admin Wallet address at account 73, external index 0, which corresponds to
								this admin key. Compare it with the expected address.
							</p>
						</div>
					) : (
						<p className="m-0">
							Plug in and unlock your Ledger and open the Bitcoin app. The Ledger may ask for permission to export the
							account public key.
						</p>
					)}
				</Step>
				<Step
					number={2}
					title="Settings"
					done={deployment !== null}
					actions={
						<Button size="sm" variant={next === 2 ? 'primary' : 'secondary'} onClick={onSettings}>
							{deployment ? 'Change settings' : 'Open settings'}
						</Button>
					}
				>
					{!deployment && <p className="m-0 mb-1">Set the signers.toml path, Strata RPC URL and Electrum server.</p>}
					<div className="text-[#0a0a0a]">
						{deployment && (
							<KeyValue label="Deployment">
								{deployment.network} · {deployment.threshold} of {deployment.adminKeys.length} admins
							</KeyValue>
						)}
						<KeyValue label="signers.toml" mono={Boolean(config?.payoutSignersPath)}>
							{config?.payoutSignersPath || 'Not set'}
						</KeyValue>
						{deployment && status && (
							<KeyValue label="Electrum" mono>
								{status.electrumUrl}
							</KeyValue>
						)}
						{deployment && (
							<KeyValue label="Coordinator" mono>
								{getOrchestratorBaseUrl()}
							</KeyValue>
						)}
					</div>
					{hook.deploymentError && <Alert kind="error">{hook.deploymentError}</Alert>}
					{deployment?.warnings.map((warning) => (
						<Alert kind="warning" key={warning}>
							{warning}
						</Alert>
					))}
				</Step>
				<Step
					number={3}
					title="Sign in on Ledger"
					disabled={!deployment}
					actions={
						<Button
							size="sm"
							variant={next === 3 ? 'primary' : 'secondary'}
							disabled={!deployment}
							onClick={() => void hook.signIn(getOrchestratorBaseUrl())}
						>
							Sign in on Ledger
						</Button>
					}
				>
					{deployment ? (
						<p className="m-0">
							Your Ledger shows a message with the coordinator address and a nonce. This screen shows the same text to
							compare.{!identity && ' The app reads the Ledger identity first.'}
						</p>
					) : (
						<p className="m-0">Finish step 2 first.</p>
					)}
				</Step>
			</ol>
		</section>
	)
}
