// A complete pending transaction must offer a way to broadcast it (audit findings a4-5, a7-5).
//
// Quorum is reached and stored on the coordinator before the app broadcasts. When that last step
// never ran (the response was lost, the app was closed) the card showed "Signed" and nothing
// else: Retry appeared only next to a recorded broadcast error. This runner cannot mount React,
// so the test pins the condition in front of each control by reading the source.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const card = readFileSync(join(here, '..', '..', 'components', 'pending-transaction-card.tsx'), 'utf8')

// Two controls call onRetryBroadcast: one after a failed broadcast, one for a complete
// transaction with no recorded failure. Together they cover every complete pending transaction.
const guards = card.split('\n').filter((line) => line.trimStart().startsWith('{tx.') && line.trimEnd().endsWith('&& ('))
assert.ok(
	guards.some((g) => g.trim() === '{tx.broadcastError && ('),
	'Retry broadcast stays next to a broadcast error',
)
assert.ok(
	guards.some((g) => g.trim() === '{tx.complete && !tx.broadcastError && ('),
	'a complete transaction without a recorded error offers Broadcast',
)
assert.equal(card.split('onClick={onRetryBroadcast}').length - 1, 2, 'both controls broadcast')

// The full outpoint of each input is readable (PRD 06 §6.b: the specific inputs).
assert.ok(card.includes('title={input.outpoint}'), 'the truncated input shows its full outpoint on hover')

console.log('pending-card-broadcast: ok')
