type ReceiveIdentity = { receiveIndex: number; receiveAddress: string; receiveOnDevice: string | null }

export function matchesReceiveAddress(started: ReceiveIdentity, current: ReceiveIdentity, address: string): boolean {
	return (
		started.receiveIndex === current.receiveIndex &&
		started.receiveAddress === current.receiveAddress &&
		address === (current.receiveOnDevice ?? current.receiveAddress)
	)
}
