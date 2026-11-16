import { useState } from 'react'
import { payoutPagination } from '../model/payout-pagination'

export function usePayoutPagination(count: number, pageSize: number) {
	const [requestedPage, setPage] = useState(1)
	const pagination = payoutPagination(requestedPage, count, pageSize)
	// Adjust stored state too, so later growth does not jump back to an obsolete page.
	if (requestedPage !== pagination.page) setPage(pagination.page)
	return { ...pagination, setPage }
}
