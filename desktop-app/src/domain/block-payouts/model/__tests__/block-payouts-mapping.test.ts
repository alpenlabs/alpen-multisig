import assert from 'node:assert/strict'
import { feeRateError } from '../../../../api/payouts.ts'
import { amountUnit, formatAmount, formatTimeRemaining, groupsWithout } from '../block-payouts.mapping.ts'

// Fee rate: 0.1–10,000 sat/vB in 0.1 steps.
assert.equal(feeRateError('0.1'), null)
assert.equal(feeRateError('10000'), null)
assert.equal(feeRateError('2.5'), null)
assert.notEqual(feeRateError('0'), null)
assert.notEqual(feeRateError('10000.1'), null)
assert.notEqual(feeRateError('1.25'), null)
assert.notEqual(feeRateError('abc'), null)
assert.notEqual(feeRateError(''), null)

// Expiry countdown.
const now = Date.UTC(2026, 10, 11)
assert.deepEqual(formatTimeRemaining(now + 4 * 86_400_000, now), { label: '4d 0h', isUrgent: false })
assert.deepEqual(formatTimeRemaining(now + 90 * 60_000, now), { label: '1h 30m', isUrgent: true })
assert.deepEqual(formatTimeRemaining(now - 1, now), { label: 'Expired', isUrgent: true })

// Removing inputs keeps the remaining transactions' shape.
assert.deepEqual(groupsWithout([['a', 'b'], ['c']], new Set(['b', 'c'])), [['a']])

// Amounts as the Ledger shows them, satoshis after.
assert.equal(formatAmount(330, 'TEST'), '0.0000033 TEST (330 sats)')
assert.equal(formatAmount(1, 'TEST'), '0.00000001 TEST (1 sats)')
assert.equal(formatAmount(100_000_000, 'BTC'), '1 BTC (100,000,000 sats)')
assert.equal(formatAmount(150_000_000, 'BTC'), '1.5 BTC (150,000,000 sats)')
assert.equal(formatAmount(0, 'TEST'), '0 TEST (0 sats)')
assert.equal(amountUnit('main'), 'BTC')
assert.equal(amountUnit('regtest'), 'TEST')
