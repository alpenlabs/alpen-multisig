import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequestGeneration, isCurrentResponse, requestKey } from '../request-identity'
const request = { claims: [{ outpoint: 'a:0', covenantIndex: 0, payoutHeight: null }], feeRate: '1' }
assert.equal(requestKey(request), requestKey({ ...request }))
assert.notEqual(requestKey(request), requestKey({ ...request, feeRate: '2' }))
assert.notEqual(requestKey(request), requestKey({ ...request, claims: [] }))
assert.notEqual(requestKey(request), requestKey({ ...request, groups: [['a:0']] }))
assert.equal(isCurrentResponse(1, 2), false)
assert.equal(isCurrentResponse(2, 2), true)
console.log('request identity: ok')

const generation = createRequestGeneration()
const initial = generation.snapshot()
assert.equal(generation.isCurrent(initial), true)
for (const reason of ['input edit', 'new request', 'mutation', 'session cleanup']) {
	generation.next()
	assert.equal(generation.isCurrent(initial), false, reason)
	assert.equal(generation.isCurrent(generation.snapshot()), true, reason)
}

const planHook = readFileSync(new URL('../../hooks/use-create-block-payout.ts', import.meta.url), 'utf8')
assert.match(planHook, /clearTimeout\(feeTimer.current\)/)
assert.match(planHook, /invalidatePlan\(\)[\s\S]*const nextClaims = claims.filter/)
assert.match(planHook, /await payoutPlan\(request\)\s*if \(!planGeneration.isCurrent\(generation\)\) return/)
assert.match(planHook, /setTimeout\(\(\) => void replan\(claims, groups, rate, generation\), 300\)/)
console.log('plan response ownership and debounce wiring: ok')

const overviewHook = readFileSync(new URL('../../hooks/use-block-payouts.ts', import.meta.url), 'utf8')
const response = overviewHook.indexOf('const res = await payoutOverview()')
const guard = overviewHook.indexOf('if (!overviewGeneration.isCurrent(generation)) return', response)
assert.ok(guard > response && guard < overviewHook.indexOf('if (!res.ok)', response))
assert.match(overviewHook, /overviewPending.current === null/)
assert.match(overviewHook, /mutations.current > 0/)
assert.ok(overviewHook.includes('overviewActive.current = false'))
assert.ok(overviewHook.includes('invalidateOverview()'))
for (const call of ['payoutSign', 'payoutImport', 'payoutPasteSignatures', 'payoutRetryBroadcast']) {
	assert.ok(overviewHook.includes('await mutate(() => ' + call))
}
console.log('overview response ownership and mutation wiring: ok')
