export type DeviceOperation = {
	begin: () => symbol | null
	end: (owner: symbol) => void
	isBusy: () => boolean
}

export function canStartDeviceOperation(owner: symbol | null): boolean {
	return owner === null
}

export function ownsDeviceOperation(current: symbol | null, completing: symbol): boolean {
	return current === completing
}
