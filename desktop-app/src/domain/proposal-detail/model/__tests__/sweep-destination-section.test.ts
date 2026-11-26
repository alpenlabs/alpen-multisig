import assert from 'node:assert/strict'
import { sweepDestinationSection } from '../sweep-destination-section.ts'

const proposed = { address: 'proposed address', addressHex: 'aabb' }
const current = { address: 'current address', addressHex: 'ccdd' }

assert.equal(sweepDestinationSection({ safeHarborChange: null, safeHarborProposed: null }), null)
assert.deepEqual(sweepDestinationSection({ safeHarborChange: null, safeHarborProposed: proposed }), {
	kind: 'proposed',
	proposed,
})

for (const from of [current, null]) {
	const change = { from, to: proposed }

	for (const safeHarborProposed of [proposed, null]) {
		assert.deepEqual(sweepDestinationSection({ safeHarborChange: change, safeHarborProposed }), {
			kind: 'change',
			change,
		})
	}
}
