import assert from 'node:assert/strict'
import { cancelMatchesProposal } from '../cancel-matches-proposal'

const stored = { targetActionId: 'proposal-a', actionHex: '0xAbCd' }

assert.equal(cancelMatchesProposal(stored, 'proposal-a', 'abcd'), true)
assert.equal(cancelMatchesProposal(stored, 'proposal-a', '0XABCE'), false)
assert.equal(cancelMatchesProposal(stored, 'proposal-b', 'abcd'), false)
assert.equal(cancelMatchesProposal({ ...stored, targetActionId: null }, 'proposal-a', 'abcd'), false)
