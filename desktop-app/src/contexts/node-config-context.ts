import { createContext } from 'react'
import type { LocalNodeStatus, NodeConfig } from '@/domain/node-config/model/node-config.types'

export type NodeConfigContextValue = {
	config: NodeConfig | null
	localNodeStatus: LocalNodeStatus | null
	isLoading: boolean
	isSaving: boolean
	localNodeUnreachable: boolean
	/** Resolves to the error message when the settings could not be saved, else to null. */
	saveConfig: (draft: NodeConfig) => Promise<string | null>
	recheck: (config: NodeConfig) => void
}

export const NodeConfigContext = createContext<NodeConfigContextValue | null>(null)
