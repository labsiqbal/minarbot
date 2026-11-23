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

export function preloadWorkflows(owner, includeLibrary) {
  if (includeLibrary) {
    const now = performance.now();
    if (pendingLibrary?.owner !== owner || pendingLibrary.expires <= now) {
      pendingLibrary = { owner, expires: now + 3000, response: fetchLibrary(owner) };
    }
  }
  return loadWorkflows().catch(() => {});
}

export async function mountRoute(host, kind, first, second) {
  if (kind === 'workflows') {
    const prefetched = pendingLibrary;
    pendingLibrary = undefined;
    const library = prefetched?.owner === second && performance.now() < prefetched.expires
      ? prefetched.response : fetchLibrary(second);
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
