import type { ComponentProps, ReactNode } from 'react'

export function ReviewButton(props: Omit<ComponentProps<'button'>, 'type'>) {
	return <button type="button" {...props} />
}

export function ReviewRow({
	label,
	value,
	stacked = false,
	mono = false,
}: {
	label: string
	value: ReactNode
	stacked?: boolean
	mono?: boolean
}) {
	if (stacked)
		return (
			<>
				<dt>{label}</dt>
				<dd className={mono ? 'm-0 break-all font-mono' : 'm-0'}>{value}</dd>
			</>
		)
	return (
		<div className="flex items-center justify-between gap-3">
			<dt className="text-label text-[#6b7280]">{label}</dt>
			<dd className="m-0 text-body-sm font-medium text-[#0a0a0a]">{value}</dd>
		</div>
	)
}

export function Actions({ children }: { children: React.ReactNode }) {
	return <div className="flex items-center justify-between gap-2.5">{children}</div>
}

type ActionButtonProps = Pick<ComponentProps<'button'>, 'children' | 'disabled' | 'onClick'>

export function PrimaryButton(props: ActionButtonProps) {
	return (
		<ReviewButton
			className="rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a] disabled:cursor-not-allowed disabled:opacity-50"
			{...props}
		/>
	)
}

export function SecondaryButton(props: ActionButtonProps) {
	return (
		<ReviewButton
			className="rounded-lg border border-[#e5e7eb] bg-white px-4 py-2 text-body-sm font-medium text-[#374151] transition hover:bg-[#f9fafb] disabled:opacity-50"
			{...props}
		/>
	)
}
