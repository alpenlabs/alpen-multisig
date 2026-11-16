import assert from 'node:assert/strict'
import { matchesReceiveAddress } from '../wallet-verification'
const first = { receiveIndex: 0, receiveAddress: 'bcrt1first', receiveOnDevice: 'tb1first' }
assert.equal(matchesReceiveAddress(first, first, 'tb1first'), true)
assert.equal(matchesReceiveAddress(first, first, 'wrong'), false)
assert.equal(matchesReceiveAddress(first, { ...first, receiveIndex: 1 }, 'tb1first'), false)
assert.equal(matchesReceiveAddress(first, { ...first, receiveAddress: 'bcrt1second' }, 'tb1first'), false)
const mainnet = { receiveIndex: 2, receiveAddress: 'bc1address', receiveOnDevice: null }
assert.equal(matchesReceiveAddress(mainnet, mainnet, 'bc1address'), true)
console.log('wallet verification identity: ok')
