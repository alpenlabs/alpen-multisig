import assert from 'node:assert/strict'
import { manualSignBlockedReason } from '../manual-sign-blocked-reason.ts'

for (const vendor of ['ledger', 'trezor', 'mnemonic', 'mock'] as const) {
	for (const displayKind of ['none', 'text', 'hash-and-text'] as const) {
		assert.ok(manualSignBlockedReason(false, vendor, displayKind))

		const reason = manualSignBlockedReason(true, vendor, displayKind)

		if ((vendor === 'ledger' || vendor === 'trezor') && displayKind === 'none') {
			assert.ok(reason)
		} else {
			assert.equal(reason, null)
		}
	}
}
