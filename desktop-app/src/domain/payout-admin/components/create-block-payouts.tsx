import { useEffect } from 'react'
import { Button } from '@/components/button'
import { useCreateBlockPayouts } from '../hooks/use-create-block-payouts'
import type { PayoutAdminHook } from '../hooks/use-payout-admin'
import { Alert, Working } from './payout-ui'

const SIZE_LIMIT_VB = 10000

function sats(value: number): string {
	return `${value.toLocaleString('en-US')} sats`
}

/** The Block payouts page: paste false-claim reports, check the fee, then confirm each transaction on the Ledger. */
export function CreateBlockPayouts({ hook }: { hook: PayoutAdminHook }) {
	const form = useCreateBlockPayouts()
	const { reports, transactions, confirmedCount } = form
	const { finishCreate, busy } = hook
	const allConfirmed = 0 < confirmedCount && form.bundle !== null && transactions.length === 0

	// Nothing is left to confirm: go back to the pending list, where the new transactions wait for signatures.
	useEffect(() => {
		if (allConfirmed && !busy) finishCreate(confirmedCount)
	}, [allConfirmed, busy, confirmedCount, finishCreate])

	const valid = reports?.reports.filter((report) => report.verdict === 'valid').length ?? 0
	const excluded = reports?.reports.filter((report) => report.verdict !== 'valid') ?? []
	const spent = excluded.filter((report) => report.verdict === 'spent').length
	const unreadable = reports?.malformed ?? []

	return (
		<section className="mx-auto w-full max-w-200 rounded-xl border border-[#e5e7eb] bg-white p-6">
			<div className="flex items-center justify-between gap-4">
				<h1 className="m-0 font-display text-display-md font-normal text-[#0a0a0a]">Block payouts</h1>
				<Button size="sm" variant="secondary" onClick={hook.closeModal}>
					{confirmedCount === 0 ? 'Cancel' : 'Done'}
				</Button>
			</div>
			{hook.error && (
				<Alert kind="error" onDismiss={hook.dismissError}>
					{hook.error}
				</Alert>
			)}

			<h2 className="m-0 mb-2 mt-5 text-body font-semibold text-[#0a0a0a]">1. False-claim reports</h2>
			<textarea
				aria-label="False-claim report JSONs"
				className="h-28 w-full resize-y rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3 font-mono text-label text-[#0a0a0a] outline-none transition focus:border-[#0a0a0a] focus:bg-white"
				placeholder="Paste one or more false-claim report JSONs"
				value={form.text}
				onChange={(event) => form.setText(event.target.value)}
				spellCheck={false}
			/>
			<div className="mt-2 flex flex-wrap items-center gap-3">
				<Button
					size="sm"
					variant={reports ? 'secondary' : 'primary'}
					disabled={!form.text.trim()}
					onClick={() => void hook.run(form.verify, 'Verifying the reports…')}
				>
					Verify reports
				</Button>
				{reports && (
					<span className="text-body-sm text-[#0a0a0a]">
						{reports.reports.length + unreadable.length} reports:{' '}
						<span className="font-semibold text-[#059669]">{valid} valid</span>,{' '}
						<span className={excluded.length - spent + unreadable.length ? 'font-semibold text-danger-deep' : ''}>
							{excluded.length - spent + unreadable.length} excluded
						</span>
						, {spent} already spent (ignored)
					</span>
				)}
			</div>
			{0 < excluded.length + unreadable.length && (
				<Alert kind="warning">
					<p className="m-0 font-semibold">Not included</p>
					<ul className="m-0 mt-1 list-disc pl-5">
						{unreadable.map((report) => (
							<li key={`unreadable-${report.position}`}>
								Report {report.position} could not be read: {report.error}
							</li>
						))}
						{excluded.map((report) => (
							<li key={report.newClaimAdminPayoutOutpoint}>
								<span className="font-mono text-label">{report.newClaimAdminPayoutOutpoint}</span>
								<span className="block">
									{report.verdict === 'spent' ? 'Already spent in a block. ' : ''}
									{report.reasons.join('; ')}
								</span>
							</li>
						))}
					</ul>
				</Alert>
			)}

			{reports && (
				<>
					<h2 className="m-0 mb-2 mt-6 text-body font-semibold text-[#0a0a0a]">2. Transactions</h2>
					<div className="flex flex-wrap items-center gap-3 text-body-sm text-[#0a0a0a]">
						<label className="flex items-center gap-2">
							Fee rate (sat/vB)
							<input
								type="number"
								min="0.1"
								max="10000"
								step="0.1"
								className="w-24 rounded-lg border border-[#e5e7eb] px-3 py-1.5 outline-none focus:border-[#0a0a0a]"
								value={form.fee}
								onChange={(event) => form.setFee(event.target.value)}
							/>
						</label>
						{form.previewing && <Working label="Updating the fee preview…" />}
						{form.bundle && 0 < transactions.length && (
							<span>
								Total fee:{' '}
								<span className="font-semibold">{sats(transactions.reduce((sum, tx) => sum + tx.feeSat, 0))}</span>
							</span>
						)}
					</div>
					{form.rate === null && <Alert kind="error">Use 0.1–10,000 sat/vB in 0.1 steps.</Alert>}
					{form.previewError && (
						<Alert kind="error">
							<p className="m-0">{form.previewError}</p>
							<Button size="sm" variant="secondary" className="mt-2" onClick={() => void form.retryPreview()}>
								Try again
							</Button>
						</Alert>
					)}
					{0 < confirmedCount && (
						<Alert kind="success">
							{confirmedCount === 1
								? '1 block payouts transaction was created with your signature.'
								: `${confirmedCount} block payouts transactions were created with your signature.`}
						</Alert>
					)}
					{form.bundle && transactions.length === 0 && confirmedCount === 0 && (
						<p className="m-0 mt-3 text-body-sm text-[#6b7280]">No valid report is left to block.</p>
					)}
					{transactions.map((tx, i) => (
						<div key={tx.transactionId} className="mt-3 rounded-xl border border-[#e5e7eb] bg-bg-base p-4">
							<div className="flex flex-wrap items-baseline justify-between gap-2 text-body-sm text-[#0a0a0a]">
								<span className="font-semibold">
									Transaction {i + 1} of {transactions.length}
								</span>
								<span>
									{tx.claimInputCount} claim input{tx.claimInputCount === 1 ? '' : 's'} ·{' '}
									{tx.vsize.toLocaleString('en-US')} vB · Fee {sats(tx.feeSat)} · Change {sats(tx.changeSat)}
								</span>
							</div>
							{tx.errors.map((error) => (
								<Alert kind="error" key={error}>
									{error}
								</Alert>
							))}
							{SIZE_LIMIT_VB < tx.vsize && (
								<Alert kind="error">Critical: transaction exceeds the 10,000 vB size limit.</Alert>
							)}
							{15 <= tx.claimInputCount && (
								<Alert kind="warning">Ledger PIN lock may interrupt signing from 15 claims.</Alert>
							)}
							<ol className="m-0 mt-2 list-decimal pl-6 font-mono text-label leading-5 text-[#374151]">
								{tx.claims.map((claim) => (
									<li key={claim.outpoint} className="py-0.5">
										<span className="wrap-anywhere">{claim.outpoint}</span>{' '}
										<button
											type="button"
											className="font-sans text-accent hover:text-accent-hover hover:underline"
											onClick={() => void hook.run(() => form.remove(claim.outpoint), 'Updating the transactions…')}
										>
											Remove input
										</button>
									</li>
								))}
							</ol>
							<div className="mt-3 flex justify-end">
								<Button
									size="sm"
									disabled={0 < tx.errors.length || SIZE_LIMIT_VB < tx.vsize || tx.claimInputCount === 0}
									onClick={() =>
										void hook.reviewTransaction(form.bundle!.bundleId, tx.transactionId, () => form.confirmed(tx))
									}
								>
									Confirm
								</Button>
							</div>
						</div>
					))}
				</>
			)}
		</section>
	)
}
