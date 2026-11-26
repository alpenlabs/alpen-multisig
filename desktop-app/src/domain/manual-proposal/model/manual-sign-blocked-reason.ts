import type { DeviceSigningDisplay } from '@/lib/device-signing-display'
import type { WalletVendor } from '@/wallet/types'

export function manualSignBlockedReason(
	hasCurrentDecode: boolean,
	vendor: WalletVendor,
	displayKind: DeviceSigningDisplay['kind'],
): string | null {
	if (!hasCurrentDecode) {
		return 'The imported action must be decoded before signing.'
	}

	if ((vendor === 'ledger' || vendor === 'trezor') && displayKind === 'none') {
		return 'Waiting for the device message preview.'
	}

	return null
}
