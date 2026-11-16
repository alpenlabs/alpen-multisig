export function payoutPagination(requestedPage: number, count: number, pageSize: number) {
	const totalPages = Math.max(1, Math.ceil(count / pageSize))
	const page = Math.max(1, Math.min(requestedPage, totalPages))
	return { page, totalPages, start: (page - 1) * pageSize, end: page * pageSize }
}
