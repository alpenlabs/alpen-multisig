import { useState } from 'react'
import { Button } from '@/components/button'
import { PayoutModal } from './payout-modal'
import { Alert } from './payout-ui'

type Props = {
	verifyReport?: boolean
	error: string | null
	/** Resolves to true when the PSBT was imported. */
	onImport: (text: string) => Promise<boolean>
	onClose: () => void
}

export function ImportPsbtModal({ verifyReport, error, onImport, onClose }: Props) {
	const [text, setText] = useState('')

	async function handleImport() {
		if (await onImport(text)) onClose()
	}

	return (
		<PayoutModal onClose={onClose}>
			<h2 className="m-0 font-display text-display-sm font-normal text-[#0a0a0a]">
				{verifyReport ? 'Verify with report' : 'Import PSBT'}
			</h2>
			<p className="m-0 mt-1 text-body-sm text-[#6b7280]">
				{verifyReport
					? 'Paste one version 2 report covering every claim in this transaction.'
					: 'Paste a block payouts transaction as a base64 PSBT, with or without signatures. It becomes pending at once.'}
			</p>
			<textarea
				aria-label={verifyReport ? 'False claim report' : 'Base64 PSBT'}
				className="mt-4 h-36 w-full resize-none rounded-xl border border-[#e5e7eb] bg-bg-base px-4 py-3 font-mono text-label text-[#0a0a0a] outline-none transition focus:border-[#0a0a0a] focus:bg-white"
				placeholder={verifyReport ? undefined : 'cHNidP8B…'}
				value={text}
				onChange={(event) => setText(event.target.value)}
				spellCheck={false}
			/>
			{error && <Alert kind="error">{error}</Alert>}
			<div className="mt-4 flex justify-end gap-2">
				<Button size="sm" variant="secondary" onClick={onClose}>
					Cancel
				</Button>
				<Button size="sm" disabled={!text.trim()} onClick={() => void handleImport()}>
					{verifyReport ? 'Verify' : 'Import'}
				</Button>
			</div>
		</PayoutModal>
	)
}
