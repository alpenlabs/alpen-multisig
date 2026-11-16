import { PayoutError } from './payout-error'
import { PayoutButton } from './payout-button'
import { PayoutModal } from './payout-modal'
import { useRef } from 'react'
import type { CreateBlockPayoutHook } from '../hooks/use-create-block-payout'
import { formatAmount } from '../model/block-payouts.mapping'
import { ReportSummary } from './report-summary'
import { truncate } from './modal-utils'
import { ReviewRow as Row, Actions, PrimaryButton, SecondaryButton } from './review-controls'

type Props = {
	unit: string
	flow: CreateBlockPayoutHook
	onClose: () => void
}

const STEPS = ['reports', 'review'] as const

export function CreateBlockPayoutModal({ unit, flow, onClose }: Props) {
	const fileRef = useRef<HTMLInputElement>(null)
	const { step, plan } = flow
	const stepIndex = STEPS.indexOf(step)
	const pendingOutpoints = new Set(flow.reports.filter((r) => r.inPending).map((r) => r.outpoint))

	function handleFileLoad(e: React.ChangeEvent<HTMLInputElement>) {
		const file = e.target.files?.[0]
		if (!file) return
		const reader = new FileReader()
		reader.onload = (ev) => flow.setRawReports(String(ev.target?.result ?? ''))
		reader.readAsText(file)
	}

	return (
		<PayoutModal wide onClose={flow.isBusy ? () => undefined : onClose}>
			<div className="flex flex-col gap-5">
				<div>
					<div className="mb-3 flex items-center gap-1.5">
						{STEPS.map((s, i) => (
							<div
								key={s}
								className="h-1 flex-1 rounded-full transition-all"
								style={{ background: i <= stepIndex ? '#0a0a0a' : '#e5e7eb' }}
							/>
						))}
					</div>
					<h2 className="m-0 font-display text-display-sm font-normal text-[#0a0a0a]">
						{step === 'reports' && 'Block payouts: false claim reports'}
						{step === 'review' && 'Review transactions and fee'}
					</h2>
					<p className="m-0 mt-1 text-label text-[#6b7280]">
						Step {stepIndex + 1} of {STEPS.length} · Payout Administrator
					</p>
				</div>

				<PayoutError>{flow.error}</PayoutError>

				{step === 'reports' && (
					<>
						<div>
							<p className="m-0 mb-2 text-body-sm text-[#374151]">
								Paste one or more false claim reports (JSON), or load a file. Each report is checked against your
								Bitcoin Core node; invalid reports and claims already spent are ignored.
							</p>
							<textarea
								className="h-44 w-full resize-none rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3 font-mono text-label text-[#0a0a0a] outline-none transition focus:border-[#0a0a0a] focus:bg-white"
								placeholder={
									'{"operatorPubkey":"…","newClaimTxid":"…","newContestTxid":"…","ackedClaimTxid":"…","ackedContestTxid":"…","counterproofAckTxid":"…"}'
								}
								value={flow.rawReports}
								onChange={(e) => flow.setRawReports(e.target.value)}
								spellCheck={false}
								aria-label="False claim reports"
							/>
							<PayoutButton appearance="load" onClick={() => fileRef.current?.click()}>
								Load from file
							</PayoutButton>
							<input ref={fileRef} type="file" accept=".json,.txt" className="hidden" onChange={handleFileLoad} />
						</div>
						<Actions>
							<SecondaryButton onClick={onClose}>Cancel</SecondaryButton>
							<PrimaryButton
								disabled={flow.rawReports.trim().length === 0 || flow.isBusy}
								onClick={() => void flow.handleVerify()}
							>
								{flow.isBusy ? 'Checking reports…' : 'Next'}
							</PrimaryButton>
						</Actions>
					</>
				)}

				{step === 'review' && (
					<>
						<ReportSummary reports={flow.reports} ignoredCount={flow.ignoredCount} malformed={flow.malformed} />

						<div>
							<label className="mb-1.5 block text-body-sm font-medium text-[#374151]" htmlFor="payout-fee-rate">
								Fee rate (sat/vB)
							</label>
							<input
								id="payout-fee-rate"
								inputMode="decimal"
								value={flow.feeRate}
								onChange={(e) => flow.setFeeRate(e.target.value)}
								className="w-full rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-2.5 text-body-sm text-[#0a0a0a] outline-none transition focus:border-[#0a0a0a] focus:bg-white"
							/>
							<p className={`m-0 mt-1.5 text-label ${flow.feeRateMessage ? 'text-danger-deep' : 'text-[#6b7280]'}`}>
								{flow.feeRateMessage ?? '0.1 – 10,000 sat/vB, in steps of 0.1. Paid from your Admin Wallet.'}
							</p>
						</div>

						{plan && (
							<div className="flex max-h-72 flex-col gap-3 overflow-y-auto">
								{plan.txs.map((t, i) => (
									<div key={t.claims[0]?.outpoint ?? i} className="rounded-xl border border-[#e5e7eb] bg-bg-base p-3">
										<div className="mb-2 flex items-center justify-between gap-2">
											<span className="text-body-sm font-medium text-[#0a0a0a]">
												Transaction {i + 1} · {t.claims.length} claim input{t.claims.length !== 1 ? 's' : ''}
											</span>
											<span className="text-label text-[#6b7280]">
												{t.vsize.toLocaleString('en-US')} vB · fee {formatAmount(t.feeSat, unit)}
											</span>
										</div>
										{t.errors.map((e) => (
											<p key={e} className="m-0 mb-2 text-body-sm font-medium text-danger-deep" role="alert">
												{e}
											</p>
										))}
										<div className="flex flex-col gap-1">
											{t.claims.map((c) => (
												<div
													key={c.outpoint}
													className="flex items-center justify-between gap-3 rounded-lg bg-white px-3 py-1.5 shadow-sm"
												>
													<span className="flex min-w-0 items-center gap-2">
														<span className="font-mono text-label text-[#374151]" title={c.outpoint}>
															{truncate(c.outpoint)}
														</span>
														{pendingOutpoints.has(c.outpoint) && (
															<span
																className="shrink-0 rounded-full bg-highlight-surface px-2 py-0.5 text-label text-emphasis"
																title="Another Payout Administrator's pending transaction already spends this claim. Remove it unless that transaction is abandoned."
															>
																In Pending
															</span>
														)}
													</span>
													<PayoutButton
														aria-label={`Remove input ${c.outpoint}`}
														appearance="remove"
														onClick={() => flow.handleRemoveInput(c.outpoint)}
													>
														Remove
													</PayoutButton>
												</div>
											))}
										</div>
										<p className="m-0 mt-2 text-label text-[#6b7280]">
											Fee inputs: {t.feeCoins.length} (
											{formatAmount(
												t.feeCoins.reduce((sum, coin) => sum + coin.valueSat, 0),
												unit,
											)}
											) · change {formatAmount(t.changeSat, unit)}
										</p>
									</div>
								))}
							</div>
						)}

						{plan && (
							<div className="rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3">
								<Row label="Total fee" value={formatAmount(plan.totalFeeSat, unit)} />
								<Row label="Admin Wallet (confirmed, unreserved)" value={formatAmount(plan.availableSat, unit)} />
							</div>
						)}

						<Actions>
							<SecondaryButton onClick={flow.handleBack}>Back</SecondaryButton>
						</Actions>
					</>
				)}
			</div>
		</PayoutModal>
	)
}
