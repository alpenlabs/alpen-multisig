import { useCreateBlockPayouts } from '../hooks/use-create-block-payouts'
import type { PayoutAdminHook } from '../hooks/use-payout-admin'
import { PayoutModal } from './payout-modal'

export function CreateBlockPayoutsModal({ hook }: { hook: PayoutAdminHook }) {
	const form = useCreateBlockPayouts()

	return (
		<PayoutModal onClose={hook.closeModal} wide>
			<h2 className="font-display text-display-sm">Block Payouts</h2>
			<label>
				Paste false-claim report JSONs
				<textarea
					className="h-32 w-full rounded-xl border p-3 font-mono"
					value={form.text}
					onChange={(event) => form.setText(event.target.value)}
				/>
			</label>
			<button disabled={!form.text.trim()} onClick={() => void hook.run(form.verify)}>
				Verify reports
			</button>
			<p>Counterproof Ack requires at least 6 confirmations. Invalid and spent reports are excluded.</p>
			{form.reports?.malformed.map((report, i) => (
				<p role="alert" key={i}>
					Report {report.position}: {report.error}
				</p>
			))}
			{form.reports?.reports.map((report, i) => (
				<p className="break-all" key={i}>
					{report.newClaimAdminPayoutOutpoint}: {report.verdict} {report.reasons.join('; ')}
				</p>
			))}
			<label>
				Fee rate (sat/vB)
				<input
					type="number"
					min="0.1"
					max="10000"
					step="0.1"
					value={form.fee}
					onChange={(event) => form.setFee(event.target.value)}
				/>
			</label>
			{form.rate === null && <p role="alert">Use 0.1–10,000 sat/vB in 0.1 steps.</p>}
			<button disabled={!form.reports || form.rate === null} onClick={() => void hook.run(form.rebundle)}>
				Preview fees
			</button>
			{form.bundle && <p>Total fee: {form.transactions.reduce((sum, tx) => sum + tx.feeSat, 0)} sats</p>}
			{form.transactions.map((tx) => (
				<div key={tx.transactionId} className="my-3 rounded-xl border bg-bg-base p-4">
					<p>
						{tx.claimInputCount} claim inputs · {tx.vsize} vB · Fee {tx.feeSat} sats
					</p>
					{tx.claims.map((claim) => (
						<div className="break-all" key={claim.outpoint}>
							{claim.outpoint}{' '}
							<button onClick={() => void hook.run(() => form.remove(claim.outpoint))}>Remove input</button>
						</div>
					))}
					{tx.errors.map((error) => (
						<p role="alert" className="text-danger-deep" key={error}>
							{error}
						</p>
					))}
					{tx.vsize > 10000 && <p role="alert">Critical: transaction exceeds the 10,000 vB size limit.</p>}
					{tx.claimInputCount >= 15 && <p>Ledger PIN lock may interrupt signing from 15 claims.</p>}
					<button
						disabled={tx.errors.length > 0 || tx.vsize > 10000 || tx.claimInputCount === 0}
						onClick={() =>
							void hook.reviewTransaction(form.bundle!.bundleId, tx.transactionId, () => form.confirmed(tx))
						}
					>
						Confirm
					</button>
				</div>
			))}
			<button onClick={hook.closeModal}>Close</button>
		</PayoutModal>
	)
}
