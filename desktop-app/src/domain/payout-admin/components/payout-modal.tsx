import type { ReactNode } from 'react'

export function PayoutModal({
	children,
	onClose,
	wide = false,
}: {
	children: ReactNode
	onClose: () => void
	wide?: boolean
}) {
	return (
		<div
			className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 px-4"
			onClick={(event) => {
				if (event.target === event.currentTarget) onClose()
			}}
		>
			<div
				role="dialog"
				aria-modal="true"
				aria-label="Payout Administrator"
				className={`max-h-[90vh] w-full ${wide ? 'max-w-150' : 'max-w-120'} overflow-y-auto rounded-2xl border border-[#e5e7eb] bg-white p-6 shadow-xl`}
			>
				{children}
			</div>
		</div>
	)
}
