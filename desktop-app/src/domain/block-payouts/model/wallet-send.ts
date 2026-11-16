/**
 * Whether a new chain tip calls for reading the wallet again. `attemptedTip` is the tip of the last reload that was
 * started: a reload that failed is tried again at the next block, not on every poll (each one scans the UTXO set).
 */
export function shouldReloadWallet(
	tip: number,
	walletTip: number | undefined,
	attemptedTip: number | null,
	loading: boolean,
	ledgerBusy: boolean,
) {
	return tip !== walletTip && tip !== attemptedTip && !loading && !ledgerBusy
}

