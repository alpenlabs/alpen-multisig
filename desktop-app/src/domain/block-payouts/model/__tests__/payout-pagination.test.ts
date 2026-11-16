import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { payoutPagination } from '../payout-pagination'

function visible(page: number, count: number) {
	const rows = Array.from({ length: count }, (_, index) => index)
	const result = payoutPagination(page, count, 10)
	return { ...result, rows: rows.slice(result.start, result.end) }
}
assert.deepEqual(visible(2, 11).rows, [10])
assert.equal(visible(2, 10).page, 1)
assert.equal(visible(2, 10).rows.length, 10)
assert.equal(visible(3, 20).page, 2)
assert.equal(visible(3, 20).rows.length, 10)
assert.deepEqual(visible(2, 0), { page: 1, totalPages: 1, start: 0, end: 10, rows: [] })
assert.equal(visible(0, 10).page, 1)
for (const name of ['pending-transactions-list', 'past-transactions-list']) {
	const source = readFileSync(new URL('../../components/' + name + '.tsx', import.meta.url), 'utf8')
	assert.ok(source.includes('usePayoutPagination(txs.length, PAGE_SIZE)'))
	assert.ok(source.includes('txs.slice(start, end)'))
}
const hook = readFileSync(new URL('../../hooks/use-payout-pagination.ts', import.meta.url), 'utf8')
assert.ok(hook.includes('if (requestedPage !== pagination.page) setPage(pagination.page)'))
console.log('payout pagination shrink and empty boundaries: ok')
