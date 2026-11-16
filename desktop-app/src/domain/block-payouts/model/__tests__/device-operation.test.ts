import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { canStartDeviceOperation, ownsDeviceOperation } from '../device-operation'

const signA = Symbol('A')
const signB = Symbol('B')
assert.equal(canStartDeviceOperation(null), true)
assert.equal(canStartDeviceOperation(signA), false)
assert.equal(ownsDeviceOperation(signA, signB), false)
assert.equal(ownsDeviceOperation(signB, signA), false)
assert.equal(ownsDeviceOperation(signA, signA), true)
assert.equal(ownsDeviceOperation(null, signA), false)
for (const name of ['use-block-payouts', 'use-create-block-payout', 'use-payout-admin-wallet']) {
	const hook = readFileSync(new URL('../../hooks/' + name + '.ts', import.meta.url), 'utf8')
	assert.ok(hook.includes('device.begin()'))
	assert.ok(hook.includes('device.end(owner)'))
}
const parent = readFileSync(new URL('../../hooks/use-block-payouts.ts', import.meta.url), 'utf8')
assert.ok(parent.includes('if (!deviceBusy()) setActiveModal(modal)'))
console.log('screen device operation ownership: ok')
