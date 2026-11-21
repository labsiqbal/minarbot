import { useEffect, useState } from 'react';
import type { ComputerRequest } from '../AgentComputerPanel';

export type VaultEntry = { name: string; kind: 'login' | 'secret'; revision: number; origin?: string; agents: string[]; hasTotp?: boolean; hasNotes?: boolean };

export function useVaultSettings(request: ComputerRequest, reload: number) {
  const [entries, setEntries] = useState<VaultEntry[] | null>(null), [error, setError] = useState('');
  useEffect(() => {
    let current = true;
    void request<VaultEntry[]>('/api/vault').then(rows => { if (current) { setEntries(rows); setError(''); } }, error => {
      if (current) setError(error instanceof Error ? error.message : 'Vault could not load.');
    });
    return () => { current = false; };
  }, [request, reload]);
  return { entries, setEntries, error, setError };
}
