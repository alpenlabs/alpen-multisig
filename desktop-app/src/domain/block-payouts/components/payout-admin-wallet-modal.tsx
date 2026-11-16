import { PayoutError } from './payout-error'
import { PayoutButton } from './payout-button'
import { truncate } from './modal-utils'
import { ReviewRow as Row } from './review-controls'
import { PayoutModal, PayoutHeading } from './payout-modal'
import { ReceiveAddressRow } from '@/domain/admin-wallet/components/receive-address-row'
import type { PayoutAdminWalletHook } from '../hooks/use-payout-admin-wallet'
import { formatAmount } from '../model/block-payouts.mapping'

type Props = {
	adminWallet: PayoutAdminWalletHook
	unit: string
	onClose: () => void
}

/**
 * The Admin Wallet the payout fees are paid from (PRD §4.b, §6.d.iii.a): balance, coins and the
 * next receive address with a check on the Ledger. Read from Bitcoin Core only, like the rest of
 * the payout screens, so it lists confirmed coins.
 */
export function PayoutAdminWalletModal({ adminWallet, unit, onClose }: Props) {
	const { wallet, error, stale, isLoading, deviceCheck } = adminWallet
	return (
		<PayoutModal wallet onClose={onClose}>
			<div className="flex flex-col gap-5">
				<PayoutHeading title="Admin Wallet">
					Every payout you create pays its mining fee from here, and its change returns here. Incoming unconfirmed funds
					are not shown.
				</PayoutHeading>

				{error && <PayoutError>{error}</PayoutError>}
				{!error && stale && (
					<p className="m-0 whitespace-pre-wrap text-label text-danger-deep" role="alert">
						{stale}
					</p>
				)}

				{wallet && (
					<>
						<dl className="m-0 flex flex-col gap-2 rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3.5">
							<Row label="Balance" value={formatAmount(wallet.totalSat, unit)} />
							{wallet.unconfirmedSat !== 0 && (
								<Row
									label="Unconfirmed"
									value={`${wallet.unconfirmedSat > 0 ? '+' : ''}${formatAmount(wallet.unconfirmedSat, unit)}`}
								/>
							)}
							<Row label="Available" value={formatAmount(wallet.availableSat, unit)} />
						</dl>

						<div className="flex flex-col gap-2">
							<p className="m-0 text-body-sm font-medium text-[#0a0a0a]">Receive</p>
							<ReceiveAddressRow address={wallet.receiveAddress} isLoading={isLoading} />
							{wallet.receiveOnDevice && (
								<p className="m-0 text-label text-[#6b7280]">
									Your Ledger shows this as <span className="break-all font-mono">{wallet.receiveOnDevice}</span>
								</p>
							)}
							<div className="flex items-center gap-3">
								<PayoutButton
									disabled={isLoading || deviceCheck.status === 'busy'}
									appearance="verify"
									onClick={() => void adminWallet.handleShowOnLedger()}
								>
									{deviceCheck.status === 'busy' ? 'Confirm on your Ledger…' : 'Show on Ledger'}
								</PayoutButton>
								{deviceCheck.status === 'ok' && (
									<span className="text-label font-medium text-[#059669]">The Ledger shows the same address.</span>
								)}
							</div>
							{deviceCheck.status === 'error' && (
								<p className="m-0 whitespace-pre-wrap text-label text-danger-deep" role="alert">
									{deviceCheck.message}
								</p>
							)}
						</div>

						<div className="flex flex-col gap-2">
							<p className="m-0 text-body-sm font-medium">Addresses with balance</p>
							{wallet.addresses.map((address) => (
								<div key={address.address} className="text-label">
									<p className="m-0 break-all font-mono">{address.address}</p>
									<p className="m-0">{formatAmount(address.balanceSat, unit)}</p>
								</div>
							))}
						</div>
						<div className="flex flex-col gap-1.5">
							<p className="m-0 text-body-sm font-medium text-[#0a0a0a]">Coins ({wallet.coins.length})</p>
							{wallet.coins.length === 0 && (
								<p className="m-0 text-label text-[#6b7280]">No confirmed coins. Send some to the address above.</p>
							)}
							<ul className="m-0 flex max-h-48 list-none flex-col gap-1 overflow-y-auto p-0">
								{wallet.coins.map((c) => (
									<li key={c.outpoint} className="flex items-center justify-between gap-3 text-label">
										<span className="font-mono text-[#374151]" title={c.address}>
											{truncate(c.address, 14, 6)} · {c.branch}
										</span>
										<span className="text-[#0a0a0a]">
											{formatAmount(c.valueSat, unit)}
											{c.reserved ? ' · in a pending payout' : c.inFlight ? ' · being sent' : ''}
										</span>
									</li>
								))}
							</ul>
						</div>
					</>
				)}

				<div className="flex justify-end gap-2">
					<PayoutButton
						disabled={isLoading || deviceCheck.status === 'busy'}
						appearance="refresh"
						onClick={() => void adminWallet.handleRefresh()}
					>
						{isLoading ? 'Loading…' : 'Refresh'}
					</PayoutButton>
					<PayoutButton appearance="close" onClick={onClose}>
						Close
					</PayoutButton>
				</div>
			</div>
		</PayoutModal>
	)
}
