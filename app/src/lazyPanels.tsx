import { lazy, type ReactNode } from 'react';
import type { SettingsSection } from './navigation';

const focusFailedPanelHeading = (heading: HTMLHeadingElement | null) => heading?.focus();
const panelLoadFailure = () => ({ default: () => <section className="empty"><h1 tabIndex={-1} ref={focusFailedPanelHeading}>Panel could not be loaded</h1><p role="alert">Check your connection, then reload the app.</p><button onClick={() => window.location.reload()}>Reload app</button></section> });

function lazyPanel<M, P extends object>(importModule: () => Promise<M>, select: (module: M) => (props: P) => ReactNode) {
  let component: ((props: P) => ReactNode) | undefined;
  let pending: Promise<(props: P) => ReactNode> | undefined;
  function load() {
    return pending ??= importModule().then(module => {
      component = select(module);
      return component;
    }).catch(error => {
      pending = undefined;
      throw error;
    });
  }
  const Lazy = lazy<(props: P) => ReactNode>(async () => {
    try { return { default: await load() }; }
    catch { return panelLoadFailure() as { default: (props: P) => ReactNode }; }
  });
  const Panel = (props: P) => {
    const Loaded = component ?? Lazy;
    return <Loaded {...props} />;
  };
  return Object.assign(Panel, { prefetch: () => { void load().catch(() => {}); } });
}

export const WorkspacePanel = lazyPanel(() => import('./WorkspacePanel'), module => module.WorkspacePanel);
export const MemoryPanel = lazyPanel(() => import('./MemoryPanel'), module => module.MemoryPanel);
export const WorkflowPanel = lazyPanel(() => import('./WorkflowPanel'), module => module.WorkflowPanel);
export const WorkflowDirectory = lazyPanel(() => import('./WorkflowDirectory'), module => module.WorkflowDirectory);
export const GeneralSettings = lazyPanel(() => import('./GeneralSettings'), module => module.GeneralSettings);
export const PresentationSettings = lazyPanel(() => import('./PresentationSettings'), module => module.PresentationSettings);
export const SharedDataPanel = lazyPanel(() => import('./SharedDataPanel'), module => module.SharedDataPanel);
export const UsagePanel = lazyPanel(() => import('./UsagePanel'), module => module.UsagePanel);
export const BackupPanel = lazyPanel(() => import('./BackupPanel'), module => module.BackupPanel);
export const AgentTemplateImport = lazyPanel(() => import('./AgentTemplateImport'), module => module.AgentTemplateImport);
export const AgentTemplateExport = lazyPanel(() => import('./AgentTemplateImport'), module => module.AgentTemplateExport);
export const McpServerPanel = lazyPanel(() => import('./McpServerPanel'), module => module.McpServerPanel);
export const McpPanel = lazyPanel(() => import('./McpPanel'), module => module.McpPanel);
export const HubConnection = lazyPanel(() => import('./hub/HubConnection'), module => module.HubConnection);
export const ConnectedAppsPanel = lazyPanel(() => import('./ConnectedAppsPanel'), module => module.ConnectedAppsPanel);
export const SkillsPanel = lazyPanel(() => import('./SkillsPanel'), module => module.SkillsPanel);
export const VaultSettings = lazyPanel(() => import('./VaultSettings'), module => module.VaultSettings);
export const ComputerSetupPage = lazyPanel(() => import('./ComputerSetupPage'), module => module.ComputerSetupPage);
export const CraftAnythingSettings = lazyPanel(() => import('./CraftAnythingSettings'), module => module.CraftAnythingSettings);
export const SitesPanel = lazyPanel(() => import('./SitesPanel'), module => module.SitesPanel);
export const AvatarPicker = lazyPanel(() => import('./ui/avatar-shapes'), module => module.AvatarPicker);

export const settingsPrefetch: Partial<Record<SettingsSection, () => void>> = {
  general: GeneralSettings.prefetch,
  appearance: PresentationSettings.prefetch,
  skills: SkillsPanel.prefetch,
  'connected-apps': ConnectedAppsPanel.prefetch,
  hub: HubConnection.prefetch,
  mcp: McpPanel.prefetch,
  'mcp-server': McpServerPanel.prefetch,
  usage: UsagePanel.prefetch,
  data: SharedDataPanel.prefetch,
  backup: BackupPanel.prefetch,
  vault: VaultSettings.prefetch,
  computer: ComputerSetupPage.prefetch,
  'craft-anything': CraftAnythingSettings.prefetch,
};

export const panelPrefetchers = [
  WorkflowDirectory.prefetch, WorkflowPanel.prefetch, WorkspacePanel.prefetch, MemoryPanel.prefetch,
  AgentTemplateImport.prefetch, AgentTemplateExport.prefetch, SitesPanel.prefetch, AvatarPicker.prefetch,
  ...Object.values(settingsPrefetch),
];
