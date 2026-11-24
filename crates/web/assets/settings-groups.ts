import type icons from './lucide.json';

export const settingsGroups: { label: string; items: { id: string; label: string; icon: keyof typeof icons }[] }[] = [
  { label: 'Preferences', items: [{ id: 'general', icon: 'SlidersHorizontal', label: 'General' }, { id: 'appearance', icon: 'Palette', label: 'Appearance' }] },
  { label: 'Agents', items: [{ id: 'skills', icon: 'BookOpen', label: 'Skills' }] },
  { label: 'Connections', items: [{ id: 'providers', icon: 'Server', label: 'Providers' }, { id: 'hub', icon: 'Link', label: 'minarbot.com' }, { id: 'connected-apps', icon: 'Plug', label: 'Connected apps' }, { id: 'email', icon: 'Mail', label: 'Email' }, { id: 'mcp', icon: 'Cable', label: 'MCP connections' }, { id: 'vault', icon: 'KeyRound', label: 'Vault' }] },
  { label: 'System', items: [{ id: 'machines', icon: 'Monitor', label: 'Machines' }, { id: 'devices', icon: 'Smartphone', label: 'Devices' }, { id: 'computer', icon: 'Monitor', label: 'Computer' }, { id: 'updates', icon: 'Download', label: 'Updates' }] },
  { label: 'Data', items: [{ id: 'usage', icon: 'BarChart3', label: 'Usage' }, { id: 'data', icon: 'Database', label: 'Shared data' }, { id: 'backup', icon: 'Archive', label: 'Backup & restore' }] },
  { label: 'Advanced', items: [{ id: 'mcp-server', icon: 'KeyRound', label: 'External access' }] },
];
