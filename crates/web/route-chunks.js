export const workflowMarkdown = (text, scope) =>
  import('minarbot-workflow-markdown').then(({ renderWorkflowMarkdown }) => renderWorkflowMarkdown(text, scope));

let workflows;
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

export async function mountRoute(host, kind, first, second) {
  if (kind === 'workflows') {
    const runtime = await loadWorkflows();
    if (!host.isConnected) throw new Error('Workflow outlet closed');
    globalThis.minarbotWorkflowMarkdown = workflowMarkdown;
    return runtime.mount_scoped_workflows(host, first);
  }
  const [runtime, { browserAdapter }] = await Promise.all([
    import('./conversation.js'), import('minarbot-conversation-adapter'),
  ]);
  await runtime.default();
  if (!host.isConnected) throw new Error('Step outlet closed');
  globalThis.MinarbotConversation ??= browserAdapter;
  return runtime.mount_workflow_step(host, first, second);
}
