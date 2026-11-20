import type { PayoutAdminHook } from '../hooks/use-payout-admin'
import { getOrchestratorBaseUrl } from '@/api/orchestrator-auth'
import type { NodeConfig } from '@/domain/node-config/model/node-config.types'

export function PayoutSignInPanel({
	hook,
	config,
	onSettings,
}: {
	hook: PayoutAdminHook
	config: NodeConfig | null
	onSettings: () => void
}) {
	return (
		<section className="mx-auto w-full max-w-200 rounded-xl border bg-white p-6 [&_button]:m-1 [&_button]:rounded-lg [&_button]:border [&_button]:px-3 [&_button]:py-2 [&_button:disabled]:opacity-50 [&_p]:my-2">
			<h1 className="font-display text-display-md">Payout Administrator</h1>
			<p>
				Connect your Ledger with the Bitcoin app open. Reading identity may request permission to export the account
				public key.
			</p>
			<button onClick={hook.readIdentity}>Read connected Ledger identity</button>
			{hook.identity && (
				<div className="break-all">
					<p>Admin key: {hook.identity.adminKey}</p>
					<p>Account xpub: {hook.identity.accountXpub}</p>
					<p>Fingerprint: {hook.identity.fingerprint}</p>
					<p>
						On the Ledger, review the Admin Wallet address at account 73, external index 0, which corresponds to this
						admin key.
					</p>
					<p>Expected address: {hook.identity.expectedAddress}</p>
					<button onClick={hook.showKey}>Show on device</button>
				</div>
			)}
			<button onClick={onSettings}>Node settings / payout files</button>
			<p>
				Params: {config?.payoutParamsPath || 'Not configured'} · Signers:{' '}
				{config?.payoutSignersPath || 'Not configured'}
			</p>
			<button
				disabled={!config?.payoutParamsPath || !config?.payoutSignersPath}
				onClick={() => void hook.loadDeployment(config!.payoutParamsPath!, config!.payoutSignersPath!)}
			>
				Load params.toml and signer file
			</button>
			{hook.deployment && (
				<>
					<p>
						{hook.deployment.network} · {hook.deployment.threshold} of {hook.deployment.adminKeys.length} admins
					</p>
					{hook.deployment.warnings.map((warning) => (
						<p key={warning}>{warning}</p>
					))}
					<button onClick={() => void hook.signIn(getOrchestratorBaseUrl())}>Review sign-in</button>
				</>
			)}
			{hook.toast && <p role="alert">{hook.toast}</p>}
		</section>
	)
}
