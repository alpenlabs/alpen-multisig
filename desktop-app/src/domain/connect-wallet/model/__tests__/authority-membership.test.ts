// The payout option must not wait for the ASM (audit finding a11-1): the Block Payouts screen is
// meant to work with no ASM connection, but the authority picker disabled every option, and
// Continue, while the ASM membership request was in flight.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { AuthRole } from '../../../../types/auth-role.ts'
import { needsMembershipCheck } from '../authority-membership.ts'

assert.equal(needsMembershipCheck(AuthRole.PayoutAdministrator), false, 'payouts do not depend on the ASM')
assert.equal(needsMembershipCheck(null), false, 'an option without a role has nothing to check')
for (const role of Object.values(AuthRole)) {
	if (role === AuthRole.PayoutAdministrator) continue
	assert.equal(needsMembershipCheck(role), true, `${role} is decided by the ASM membership check`)
}

// The picker cannot be mounted here (no DOM in this runner), so pin the wiring: every place that
// reacts to `isChecking` must go through the predicate.
const here = dirname(fileURLToPath(import.meta.url))
const picker = readFileSync(join(here, '..', '..', 'components', 'authority-selection-phase.tsx'), 'utf8')
const uses = picker.split('\n').filter((line) => line.includes('isChecking') && !line.includes('isChecking = false'))
assert.ok(uses.length > 0, 'the picker still reacts to isChecking')
for (const line of uses) {
	if (line.trim() === 'isChecking?: boolean' || line.trim() === 'isChecking,') continue
	assert.ok(line.includes('needsMembershipCheck'), `isChecking used without the predicate: ${line.trim()}`)
}

const hook = readFileSync(join(here, '..', '..', 'hooks', 'use-authority-membership.ts'), 'utf8')
assert.ok(hook.includes('needsMembershipCheck'), 'the hook and the picker share one definition of exempt roles')

console.log('authority-membership: ok')
