import { ReviewButton, ReviewRow } from './review-controls'
import { CopyButton } from '@/components/copy-button'
import type { PayoutWalletSendHook } from '../hooks/use-payout-wallet-send'
import type { PayoutWalletSend } from '@/api/payouts'
import { formatAmount } from '../model/block-payouts.mapping'
import { LedgerChecklist } from './ledger-checklist'

const button = 'rounded-lg border border-[#e5e7eb] px-3 py-2 text-body-sm hover:bg-[#f9fafb] disabled:opacity-50'
const field = 'w-full rounded-lg border border-[#e5e7eb] bg-white px-3 py-2 text-body-sm'

export function PayoutWalletSendPanel({
	send,
	sends,
	unit,
	disabled,
}: {
	send: PayoutWalletSendHook
	sends: PayoutWalletSend[]
	unit: string
	disabled: boolean
}) {
	const { form, review } = send
	const locked = disabled || send.busy
	return (
		<section className="flex flex-col gap-3">
			<h3 className="m-0 text-body-sm font-medium">Unconfirmed sends</h3>
			{sends.length === 0 && <p className="m-0 text-label text-[#6b7280]">No unconfirmed sends.</p>}
			{sends.map((transaction) => (
				<div key={transaction.txid} className="flex flex-col gap-2 rounded-xl border border-[#e5e7eb] p-3 text-label">
					<span className="break-all font-mono">{transaction.txid}</span>
					<span className="break-all">To: {transaction.toAddress}</span>
					<span>
						{formatAmount(transaction.amountSat, unit)} · {transaction.feeRate} sat/vB
					</span>
					<time dateTime={new Date(transaction.sentAt * 1000).toISOString()}>
						{new Date(transaction.sentAt * 1000).toLocaleString()}
					</time>
					{transaction.status === 'missing' && <p className="m-0">The node no longer has this transaction.</p>}
					<ReviewButton
						className={button}
						disabled={locked}
						onClick={() => send.handleEdit({ bumpTxid: transaction.txid, feeRate: '' })}
					>
						Bump fee
					</ReviewButton>
				</div>
			))}
			<h3 className="m-0 text-body-sm font-medium">{form.bumpTxid ? 'Bump fee' : 'Send BTC'}</h3>
			{form.bumpTxid && <p className="m-0 break-all font-mono text-label">{form.bumpTxid}</p>}
			<fieldset disabled={locked} className="m-0 flex min-w-0 flex-col gap-3 border-0 p-0">
				{!form.bumpTxid && (
					<>
						<label className="text-label">
							Send to
							<input
								className={field}
								value={form.address}
								onChange={(event) => send.handleEdit({ address: event.target.value })}
							/>
						</label>
						<label className="text-label">
							Amount (BTC)
							<input
								className={field}
								inputMode="decimal"
								value={form.max ? send.maxAmount : form.amount}
								placeholder={form.max ? 'Calculating maximum…' : ''}
								readOnly={form.max}
								onChange={(event) => send.handleEdit({ amount: event.target.value, max: false })}
							/>
						</label>
						<ReviewButton
							className={button}
							aria-pressed={form.max}
							onClick={() => send.handleEdit({ max: !form.max })}
						>
							{form.max ? 'Enter amount' : 'Max'}
						</ReviewButton>
					</>
				)}
				<label className="text-label">
					Fee rate (sat/vB)
					<input
						className={field}
						type="number"
						min="0.1"
						max="10000"
						step="0.1"
						value={form.feeRate}
						onChange={(event) => send.handleEdit({ feeRate: event.target.value })}
					/>
				</label>
				{send.noEstimate && !form.bumpTxid && (
					<p className="m-0 text-label text-[#6b7280]">The node has no fee estimate. Enter a fee rate.</p>
				)}
			</fieldset>
			{send.error && (
				<p
					role="alert"
					className="m-0 rounded-xl border border-danger-border bg-danger-surface p-3 text-body-sm text-danger-deep"
				>
					{send.error}
				</p>
			)}
			{review ? (
				<>
					<dl className="m-0 flex flex-col gap-2 rounded-xl border border-[#e5e7eb] p-3 text-label">
						<ReviewRow stacked mono label="Recipient" value={review.toAddress} />
						<ReviewRow stacked label="Amount" value={formatAmount(review.amountSat, unit)} />
						<ReviewRow
							stacked
							label="Fee"
							value={
								<>
									{formatAmount(review.feeSat, unit)} ({review.feeRate} sat/vB)
								</>
							}
						/>
						<ReviewRow stacked label="Change" value={formatAmount(review.changeSat ?? 0, unit)} />
						{review.changeAddress && <dd className="m-0 break-all font-mono">{review.changeAddress}</dd>}
					</dl>
					<LedgerChecklist review={review.review} error={null} unit={unit} />
					<ReviewButton className={button} disabled={locked || !send.canConfirm} onClick={() => void send.handleSign()}>
						{send.busy ? 'Confirm on your Ledger…' : 'Confirm on Ledger'}
					</ReviewButton>
				</>
			) : (
				<ReviewButton className={button} disabled={locked || !send.canConfirm} onClick={() => void send.handleReview()}>
					{send.busy ? 'Checking…' : 'Confirm'}
				</ReviewButton>
			)}
			{(review || form.bumpTxid) && (
				<ReviewButton className={button} disabled={locked} onClick={send.handleBack}>
					Back
				</ReviewButton>
			)}
			{send.txid && (
				<div role="status" className="rounded-xl border border-[#e5e7eb] p-3 text-label">
					<p className="m-0">Transaction sent</p>
					<p className="break-all font-mono">{send.txid}</p>
					<CopyButton text={send.txid} label="Copy transaction ID" />
				</div>
			)}
		</section>
	)
}
