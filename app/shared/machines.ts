export type PeerAgentSummary = { id: string; name: string; role?: string; model: string; status: string };
export type Environment = { id: string; name: string; version: string; origin?: string };
type ProviderAccount = { id: string; name: string; provider: string; host: string; defaultModel: string };
type AgentProvider = { provider_account: string | null; runnable: boolean; blocked_reason: string | null };
export type PairedMachine = { id: string; environmentId: string; name: string; origin: string; status: 'online' | 'stale' | 'offline' | 'unauthorized' | 'version-mismatch'; lastSeen: number | null; agentCount: number; agents: PeerAgentSummary[]; pairingState: 'paired' | 'pending'; version: string; allowAgentMessaging: boolean; remoteAgentMessaging?: boolean; allowUpdates: boolean; providerAccounts?: ProviderAccount[]; agentProviders?: Record<string, AgentProvider> };
