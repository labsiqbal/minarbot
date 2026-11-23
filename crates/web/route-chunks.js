export const workflowMarkdown = (text, scope) =>
  import('minarbot-workflow-markdown').then(({ renderWorkflowMarkdown }) => renderWorkflowMarkdown(text, scope));

let workflows;
let pendingLibrary;

function loadWorkflows() {
  workflows ??= Promise.all([
    import('./workflows.js'),
    import('minarbot-workflows-css'),
    WebAssembly.compileStreaming(fetch('__WORKFLOWS_WASM__', { signal: AbortSignal.timeout(8000) })),
  ]).then(async ([runtime, , module]) => {
    await runtime.default({ module_or_path: module });
    return runtime;
  }).catch(error => {
    workflows = undefined;
    throw error;
  });
  return workflows;
}

function fetchLibrary(owner) {
  return fetch(`/api/agents/${encodeURIComponent(owner)}/workflows`).catch(() => null);
}

function discardLibrary() {
  const pending = pendingLibrary;
  pendingLibrary = undefined;
  if (pending) {
    clearTimeout(pending.expiry);
    pending.staged.then(view => view?.dispose()).catch(() => {});
  }
}

export function preloadWorkflows(owner, includeLibrary) {
  const runtime = loadWorkflows();
  if (includeLibrary) {
    const from = window.location.pathname;
    const path = `/workflows?agent=${encodeURIComponent(owner)}&from=${encodeURIComponent(from)}&ui=leptos`;
    if (pendingLibrary?.path !== path) {
      discardLibrary();
      const response = fetchLibrary(owner);
      const pending = { path, response, expiry: setTimeout(discardLibrary, 3000) };
      pendingLibrary = pending;
      pending.staged = runtime.then(module => {
        if (pendingLibrary !== pending || window.location.pathname !== from
          || sessionStorage.getItem(`minarbot:workflow-command:${owner}`) !== null) return null;
        // Prepare the library while the menu is open. The detached host has no layout or focus.
        const content = document.createElement('div');
        content.style.display = 'contents';
        globalThis.minarbotWorkflowMarkdown = workflowMarkdown;
        return { content, dispose: module.mount_scoped_workflows(content, path, response) };
      }).catch(() => null);
    }
  }
  return runtime.catch(() => {});
}

export async function mountRoute(host, kind, first, second) {
  if (kind === 'workflows') {
    const pending = pendingLibrary?.path === first ? pendingLibrary : undefined;
    if (pending) {
      clearTimeout(pending.expiry);
      pendingLibrary = undefined;
    } else {
      discardLibrary();
    }
    const library = pending?.response ?? fetchLibrary(second);
    const staged = await pending?.staged;
    if (!host.isConnected) {
      staged?.dispose();
      throw new Error('Workflow outlet closed');
    }
    if (staged) {
      host.append(staged.content);
      if (!document.activeElement?.matches('.sidebar-settings, .sidebar-reveal')) {
        host.querySelector('h1')?.focus();
      }
      return () => {
        staged.dispose();
        staged.content.remove();
      };
    }
    const runtime = await loadWorkflows();
    if (!host.isConnected) throw new Error('Workflow outlet closed');
    globalThis.minarbotWorkflowMarkdown = workflowMarkdown;
    return runtime.mount_scoped_workflows(host, first, library);
  }
  const [runtime, { browserAdapter }] = await Promise.all([
    import('./conversation.js'), import('minarbot-conversation-adapter'),
  ]);
  await runtime.default();
  if (!host.isConnected) throw new Error('Step outlet closed');
  globalThis.MinarbotConversation ??= browserAdapter;
  return runtime.mount_workflow_step(host, first, second);
}
