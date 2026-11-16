import type { ComponentProps } from 'react'

const appearances = {
	connect:
		'mt-5 rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a] disabled:opacity-50',
	toolbar:
		'rounded-lg border border-[#e5e7eb] bg-white px-4 py-2 text-body font-medium text-[#374151] transition hover:bg-[#f9fafb]',
	create:
		'inline-flex items-center gap-1.5 rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body font-medium text-white transition hover:bg-[#2a2a2a] active:scale-[0.98]',
	tab: 'flex-1 rounded-lg py-2 text-body-sm font-medium transition',
	cancel:
		'rounded-lg border border-[#e5e7eb] bg-white px-4 py-2 text-body-sm font-medium text-[#374151] transition hover:bg-[#f9fafb]',
	sign: 'inline-flex items-center gap-1.5 rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a] active:scale-[0.98] disabled:opacity-50',
	copyId: 'rounded p-0.5 text-[#9ca3af] transition hover:text-[#6b7280]',
	retry:
		'mt-2 rounded-md border border-danger-border-soft bg-white px-3 py-1 text-label font-medium text-danger-strong transition hover:bg-danger-surface disabled:opacity-50',
	broadcast:
		'mt-2 rounded-md border border-accent-border bg-white px-3 py-1 text-label font-medium text-emphasis transition hover:bg-highlight-surface disabled:opacity-50',
	signCompact:
		'inline-flex items-center rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-3.5 py-1.5 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a] active:scale-[0.98] disabled:opacity-50',
	icon: 'flex h-7 w-7 items-center justify-center rounded-md border border-[#e5e7eb] bg-white text-[#6b7280] transition hover:border-[#d1d5db] hover:bg-[#f9fafb] hover:text-[#374151]',
	copyError:
		'mt-2.5 inline-flex items-center gap-1.5 rounded-md border border-danger-border-soft bg-white px-3 py-1 text-label font-medium text-danger-strong transition hover:bg-danger-surface',
	import:
		'inline-flex items-center rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a] active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50',
	rebroadcast:
		'inline-flex items-center gap-1.5 rounded-lg border border-[#e5e7eb] bg-white px-3 py-1.5 text-label font-medium text-[#374151] transition hover:border-[#d1d5db] hover:bg-[#f9fafb] disabled:opacity-50',
	verify:
		'rounded-lg border border-[#e5e7eb] bg-white px-3 py-1.5 text-label font-medium text-[#374151] transition hover:bg-[#f9fafb] disabled:opacity-50',
	refresh:
		'rounded-lg border border-[#e5e7eb] bg-white px-4 py-2 text-body-sm font-medium text-[#374151] transition hover:bg-[#f9fafb] disabled:opacity-50',
	close:
		'rounded-lg border border-[#0a0a0a] bg-[#0a0a0a] px-4 py-2 text-body-sm font-medium text-white transition hover:bg-[#2a2a2a]',
	load: 'mt-2 text-label text-[#6b7280] underline underline-offset-2 transition hover:text-[#374151]',
	remove:
		'shrink-0 rounded-md px-2 py-0.5 text-label text-[#9ca3af] transition hover:bg-danger-surface hover:text-danger',
}
export function PayoutButton({
	appearance,
	...props
}: Omit<ComponentProps<'button'>, 'type' | 'className'> & { appearance: keyof typeof appearances }) {
	return <button type="button" className={appearances[appearance]} {...props} />
}
