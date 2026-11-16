import assert from 'node:assert/strict'
import { formatPayoutHeldBalanceLine } from '../format-payout-held-balance-line.ts'

assert.equal(formatPayoutHeldBalanceLine(0), null)
assert.equal(formatPayoutHeldBalanceLine(50_000), '50,000 sats held by pending payouts')
assert.equal(formatPayoutHeldBalanceLine(NaN), null)

console.log('format-payout-held-balance-line: all assertions passed.')
