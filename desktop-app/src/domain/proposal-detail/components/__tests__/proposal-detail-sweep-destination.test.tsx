// ProposalDetail — a Safe Harbor update names its proposed sweep destination even when the installed one can't be
// read (H6). The manual screen and the hosted detail screen both render through this component.

import assert from 'node:assert/strict'
import { renderToStaticMarkup } from 'react-dom/server'
import { buildSyntheticProposal } from '../../../manual-proposal/model/synthetic-proposal.ts'
import { NOTHING_DECODED } from '../../hooks/use-decoded-proposal.ts'
import { ProposalDetail } from '../proposal-detail.tsx'

const PROPOSED = { address: 'bcrt1pproposeddestination', addressHex: '0451aa00' }
const INSTALLED = { address: 'bcrt1pinstalleddestination', addressHex: '0451bb11' }

const PROPOSAL = buildSyntheticProposal({
	importData: {
		actionHex: 'aa',
		seqNo: 1,
		authority: 'strata_admin',
		sighashHex: '00'.repeat(32),
		actionType: 'safe_harbor_address_update',
	},
	localSignatures: [],
	requiredSignatures: 2,
	hasQuorum: false,
})

function render(decodedData: typeof NOTHING_DECODED, signBlockedReason: string | null = null): string {
	return renderToStaticMarkup(
		<ProposalDetail
			proposal={PROPOSAL}
			signerPubkey={null}
			decodedData={decodedData}
			signBlockedReason={signBlockedReason}
			onSign={() => {}}
			onBroadcast={() => {}}
		/>,
	)
}

const proposedOnly = render({ ...NOTHING_DECODED, safeHarborProposed: PROPOSED })
assert.ok(proposedOnly.includes('Sweep destination'), 'the section renders without an installed destination')
assert.ok(proposedOnly.includes('Proposed'), 'the lone destination is labeled as the proposed one')
assert.ok(proposedOnly.includes(PROPOSED.address), 'the proposed address is shown')
assert.ok(proposedOnly.includes(PROPOSED.addressHex), 'the proposed BOSD descriptor is shown')
assert.ok(proposedOnly.includes('Change sweep destination'), 'the title names the action')
console.log('ProposalDetail: proposed destination without installed destination OK')

const comparison = render({
	...NOTHING_DECODED,
	safeHarborProposed: PROPOSED,
	safeHarborChange: { from: INSTALLED, to: PROPOSED },
})
assert.ok(comparison.includes('Current'), 'the comparison shows the installed destination column')
assert.ok(comparison.includes(INSTALLED.address), 'the installed address is shown')
assert.ok(comparison.includes(PROPOSED.address), 'the proposed address is shown')
console.log('ProposalDetail: installed vs proposed comparison OK')

const otherAction = render(NOTHING_DECODED)
assert.ok(!otherAction.includes('Sweep destination'), 'no section for an action without a destination')
console.log('ProposalDetail: no section for other actions OK')
assert.ok(!otherAction.includes('Proposed'), 'the Proposed label belongs to the destination section only')

const SIGNER = '02'.padEnd(66, '1')
const BLOCKED_REASON = 'Waiting for the device message preview.'

function renderForSigner(signBlockedReason: string | null): string {
	return renderToStaticMarkup(
		<ProposalDetail
			proposal={PROPOSAL}
			signerPubkey={SIGNER}
			decodedData={{ ...NOTHING_DECODED, safeHarborProposed: PROPOSED }}
			signBlockedReason={signBlockedReason}
			onSign={() => {}}
			onBroadcast={() => {}}
		/>,
	)
}

const SIGN_BUTTON = /<button[^>]*data-testid="e2e-detail-sign-button"[^>]*>/

const blocked = renderForSigner(BLOCKED_REASON)
assert.match(blocked.match(SIGN_BUTTON)?.[0] ?? '', /disabled=""/, 'a blocked Sign button is disabled')
assert.ok(blocked.includes(BLOCKED_REASON), 'the reason Sign is blocked is shown')

const ready = renderForSigner(null)
assert.ok(ready.match(SIGN_BUTTON) !== null, 'Sign is offered to a signer of a pending proposal')
assert.doesNotMatch(ready.match(SIGN_BUTTON)?.[0] ?? '', /disabled=""/, 'Sign is enabled when nothing blocks it')
console.log('ProposalDetail: Sign blocked only while a reason is given OK')
