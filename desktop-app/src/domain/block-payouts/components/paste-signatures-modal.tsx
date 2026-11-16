import { PayoutButton } from './payout-button'
import { PayoutCopyIcon } from './payout-icons'
import { PayoutModal, PayoutHeading } from './payout-modal'
import { writeClipboard } from '@/api/tauri-bridge'
import { useState } from 'react'
import type { PasteOutcome } from '../hooks/use-block-payouts'

type Props = {
	title: string
	description: string
	placeholder: string
	onSubmit: (raw: string) => Promise<PasteOutcome>
	onClose: () => void
}

/** Paste signatures (one per line) or import a transaction; the app reports what it could not use. */
export function PasteSignaturesModal({ title, description, placeholder, onSubmit, onClose }: Props) {
	const [raw, setRaw] = useState('')
	const [result, setResult] = useState<PasteOutcome | null>(null)
	const [isBusy, setIsBusy] = useState(false)

	async function handleSubmit() {
		setIsBusy(true)
		const res = await onSubmit(raw)
		setIsBusy(false)
		setResult(res)
		if (res.ok) {
			onClose()
		}
	}

	const errorMessage = result && !result.ok ? result.message : null

	function handleCopyError() {
		if (errorMessage) void writeClipboard(errorMessage)
	}

	return (
		<PayoutModal onClose={onClose}>
			<div className="flex flex-col gap-5">
				<PayoutHeading title={title}>{description}</PayoutHeading>

				<textarea
					className="h-36 w-full resize-none rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3 font-mono text-label text-[#0a0a0a] outline-none transition focus:border-[#0a0a0a] focus:bg-white"
					placeholder={placeholder}
					value={raw}
					onChange={(e) => {
						setRaw(e.target.value)
						setResult(null)
					}}
					spellCheck={false}
				/>

				{errorMessage && (
					<div className="rounded-xl border border-danger-border bg-danger-surface px-4 py-3">
						<pre className="m-0 max-h-48 overflow-y-auto whitespace-pre-wrap break-all font-mono text-label text-danger-deep">
							{errorMessage}
						</pre>
						<PayoutButton appearance="copyError" onClick={handleCopyError}>
							<PayoutCopyIcon size={12} />
							Copy error
						</PayoutButton>
					</div>
				)}

				<div className="flex items-center justify-end gap-2.5">
					<PayoutButton appearance="cancel" onClick={onClose}>
						Cancel
					</PayoutButton>
					<PayoutButton
						disabled={raw.trim().length === 0 || isBusy}
						appearance="import"
						onClick={() => void handleSubmit()}
					>
						{isBusy ? 'Importing…' : 'Import'}
					</PayoutButton>
				</div>
			</div>
		</PayoutModal>
	)
}
