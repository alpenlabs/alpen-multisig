import { PayoutError } from './payout-error'
import { PayoutButton } from './payout-button'
import { PayoutModal } from './payout-modal'
import { useRef } from 'react'
import type { CreateBlockPayoutHook } from '../hooks/use-create-block-payout'
import { ReportSummary } from './report-summary'
import { Actions, PrimaryButton, SecondaryButton } from './review-controls'

type Props = {
	flow: CreateBlockPayoutHook
	onClose: () => void
}

const STEPS = ['reports', 'review'] as const

export function CreateBlockPayoutModal({ flow, onClose }: Props) {
	const fileRef = useRef<HTMLInputElement>(null)
	const { step } = flow
	const stepIndex = STEPS.indexOf(step)
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
						<Actions>
							<SecondaryButton onClick={flow.handleBack}>Back</SecondaryButton>
						</Actions>
					</>
				)}
			</div>
		</PayoutModal>
	)
}
