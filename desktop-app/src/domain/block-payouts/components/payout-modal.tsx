import type { ReactNode } from 'react'

export function PayoutModal({
	children,
	onClose,
	wide = false,
	wallet = false,
}: {
	children: ReactNode
	onClose: () => void
	wide?: boolean
	wallet?: boolean
}) {
	return (
		<div
			className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 px-4"
			onClick={(e) => {
				if (e.target === e.currentTarget) onClose()
			}}
		>
			<div
				role={wallet ? 'dialog' : undefined}
				aria-modal={wallet ? true : undefined}
				aria-label={wallet ? 'Admin Wallet' : undefined}
				className={
					wallet
						? 'max-h-[90vh] w-full max-w-120 overflow-y-auto rounded-2xl border border-[#e5e7eb] bg-white p-6 shadow-xl'
						: `w-full ${wide ? 'max-w-150' : 'max-w-120'} rounded-2xl border border-[#e5e7eb] bg-white p-6 shadow-xl`
				}
			>
				{children}
			</div>
		</div>
	)
}

export function PayoutHeading({ title, children }: { title: string; children: ReactNode }) {
	return (
		<div>
			<h2 className="m-0 font-display text-display-sm font-normal text-[#0a0a0a]">{title}</h2>
			<p className="m-0 mt-1 text-body-sm text-[#6b7280]">{children}</p>
		</div>
	)
}
