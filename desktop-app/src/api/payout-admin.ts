/** Binding Tauri command contract (camelCase arguments and responses):
 * payout_admin_identity, payout_admin_show_key, payout_admin_load_deployment,
 * payout_admin_sign_in_review, payout_admin_sign_in, payout_admin_overview,
 * payout_admin_review_pending, payout_admin_sign_pending, payout_admin_copy_signatures,
 * payout_admin_paste_signatures, payout_admin_import_psbt, payout_admin_export_psbt,
 * payout_admin_retry_broadcast, payout_admin_rebroadcast, payout_admin_verify_reports,
 * payout_admin_bundle, payout_admin_review_transaction, payout_admin_confirm_transaction.
 * Review calls never touch the device. Sign/confirm consumes a reviewId tied to
 * the exact reviewed payload; stale reviews must be rejected by Rust. Sessions
 * and verified reports stay in Rust; times are epoch seconds, amounts sats,
 * fee rates integer tenths of sat/vB (1..100000), outpoints "txid:vout".
 */
import { z } from 'zod'
import { tauriCall } from '@/api/tauri-bridge'
import type { ApiResult } from '@/types'

const count = z.number().int().nonnegative()
const identitySchema = z.object({
	adminKey: z.string(),
	expectedAddress: z.string(),
	accountXpub: z.string(),
	fingerprint: z.string(),
})

export type PayoutIdentity = z.infer<typeof identitySchema>

const deploymentSchema = z.object({
	network: z.string(),
	threshold: count,
	adminKeys: z.array(z.string()),
	warnings: z.array(z.string()),
})

export type PayoutDeployment = z.infer<typeof deploymentSchema>

const pendingSchema = z.object({
	txid: z.string(),
	psbt: z.string(),
	expiresAtUnixSeconds: z.number().int(),
	claimInputCount: count,
	adminsSigned: count,
	signaturesNeeded: count,
	signedBy: z.array(z.string()),
	duplicateOutpoints: z.array(z.string()),
	broadcastError: z.string().nullable(),
})

export type PendingPayout = z.infer<typeof pendingSchema>

const pastSchema = z.object({
	txid: z.string(),
	rawTx: z.string(),
	status: z.enum(['unconfirmed', 'confirmed']),
	blockTimeUnixSeconds: z.number().int().nullable(),
})

export type PastPayout = z.infer<typeof pastSchema>

const overviewSchema = z.object({
	hidden: count,
	pending: z.array(pendingSchema.extend({ claimInputs: z.array(z.string()) })),
	past: z.array(pastSchema),
})

export type PayoutOverview = z.infer<typeof overviewSchema>

const signInReviewSchema = z.object({
	reviewId: z.string(),
	coordinatorUrl: z.string(),
	adminKey: z.string(),
	nonce: z.string(),
	expiresAtUnixSeconds: z.number().int(),
	deviceScreens: z.array(z.string()),
})

export type PayoutSignInReview = z.infer<typeof signInReviewSchema>

const sessionSchema = z.object({ adminKey: z.string(), expiresAtUnixSeconds: z.number().int() })

export type PayoutSession = z.infer<typeof sessionSchema>

const claimSchema = z.object({ outpoint: z.string() })

export type PayoutClaim = z.infer<typeof claimSchema>

const transactionSchema = z.object({
	transactionId: z.string(),
	claims: z.array(claimSchema),
	claimInputCount: count,
	feeSat: count,
	vsize: count,
	changeSat: count,
	errors: z.array(z.string()),
})

export type BundledPayout = z.infer<typeof transactionSchema>

const reviewSchema = z.object({
	reviewId: z.string(),
	blockPayoutsTxid: z.string(),
	claims: z.array(claimSchema),
	claimInputCount: count,
	feeSat: count,
	vsize: count,
	changeAddress: z.string(),
	changeSat: count,
	deviceScreens: z.array(z.string()),
	// True starting at 15 claim inputs; Ledger PIN lock can interrupt loading.
	pinLockWarning: z.boolean(),
	warnings: z.array(z.string()),
})

export type PayoutReview = z.infer<typeof reviewSchema>

const pasteSchema = z.object({
	pending: pendingSchema.nullable(),
	added: count,
	alreadyPresent: count,
	invalid: z.array(z.object({ raw: z.string(), reason: z.string() })),
})

export type PayoutPasteResult = z.infer<typeof pasteSchema>

const broadcastSchema = z.object({ txid: z.string(), broadcastError: z.string().nullable() })

export type PayoutBroadcastResult = z.infer<typeof broadcastSchema>

const reportsSchema = z.object({
	verificationId: z.string(),
	reports: z.array(
		z.object({
			newClaimAdminPayoutOutpoint: z.string(),
			verdict: z.enum(['valid', 'invalid', 'spent']),
			reasons: z.array(z.string()),
		}),
	),
	malformed: z.array(z.object({ position: count, error: z.string() })),
})

export type PayoutReports = z.infer<typeof reportsSchema>

const bundleSchema = z.object({ bundleId: z.string(), transactions: z.array(transactionSchema) })

export type PayoutBundle = z.infer<typeof bundleSchema>

export function payoutIdentity(): Promise<ApiResult<PayoutIdentity>> {
	return tauriCall('payout_admin_identity', {}, identitySchema)
}

export function payoutShowKey(): Promise<ApiResult<PayoutIdentity>> {
	return tauriCall('payout_admin_show_key', {}, identitySchema)
}

export function payoutLoadDeployment(paramsPath: string, signersPath: string): Promise<ApiResult<PayoutDeployment>> {
	return tauriCall('payout_admin_load_deployment', { paramsPath, signersPath }, deploymentSchema)
}

export function payoutSignInReview(coordinatorUrl: string): Promise<ApiResult<PayoutSignInReview>> {
	return tauriCall('payout_admin_sign_in_review', { coordinatorUrl }, signInReviewSchema)
}

export function payoutSignIn(reviewId: string): Promise<ApiResult<PayoutSession>> {
	return tauriCall('payout_admin_sign_in', { reviewId }, sessionSchema)
}

export function payoutOverview(): Promise<ApiResult<PayoutOverview>> {
	return tauriCall('payout_admin_overview', {}, overviewSchema)
}

export function payoutReviewPending(txid: string): Promise<ApiResult<PayoutReview>> {
	return tauriCall('payout_admin_review_pending', { txid }, reviewSchema)
}

export function payoutSignPending(reviewId: string): Promise<ApiResult<PayoutPasteResult>> {
	return tauriCall('payout_admin_sign_pending', { reviewId }, pasteSchema)
}

// Returns text for the existing clipboard helper; Rust does not access the clipboard.
export function payoutCopySignatures(txid: string): Promise<ApiResult<string>> {
	return tauriCall('payout_admin_copy_signatures', { txid }, z.string())
}

export function payoutPasteSignatures(txid: string, signatures: string): Promise<ApiResult<PayoutPasteResult>> {
	return tauriCall('payout_admin_paste_signatures', { txid, signatures }, pasteSchema)
}

export function payoutImportPsbt(psbt: string): Promise<ApiResult<PendingPayout>> {
	return tauriCall('payout_admin_import_psbt', { psbt }, pendingSchema)
}

export function payoutExportPsbt(txid: string): Promise<ApiResult<string>> {
	return tauriCall('payout_admin_export_psbt', { txid }, z.string())
}

export function payoutRetryBroadcast(txid: string): Promise<ApiResult<PayoutBroadcastResult>> {
	return tauriCall('payout_admin_retry_broadcast', { txid }, broadcastSchema)
}

export function payoutRebroadcast(txid: string): Promise<ApiResult<PayoutBroadcastResult>> {
	return tauriCall('payout_admin_rebroadcast', { txid }, broadcastSchema)
}

export function payoutVerifyReports(text: string): Promise<ApiResult<PayoutReports>> {
	return tauriCall('payout_admin_verify_reports', { text }, reportsSchema)
}

// Removing inputs re-bundles the verified set. Rust rechecks chain state and reserves fee
// coins on confirm, using the existing Admin Wallet's first unused change address.
export function payoutBundle(
	verificationId: string,
	feeRateTenths: number,
	removedInputs: string[],
): Promise<ApiResult<PayoutBundle>> {
	return tauriCall('payout_admin_bundle', { verificationId, feeRateTenths, removedInputs }, bundleSchema)
}

export function payoutReviewTransaction(bundleId: string, transactionId: string): Promise<ApiResult<PayoutReview>> {
	return tauriCall('payout_admin_review_transaction', { bundleId, transactionId }, reviewSchema)
}

export function payoutConfirmTransaction(reviewId: string): Promise<ApiResult<PayoutPasteResult>> {
	return tauriCall('payout_admin_confirm_transaction', { reviewId }, pasteSchema)
}
