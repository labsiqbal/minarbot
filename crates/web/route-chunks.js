export const workflowMarkdown = (text, scope) =>
  import('minarbot-workflow-markdown').then(({ renderWorkflowMarkdown }) => renderWorkflowMarkdown(text, scope));

export async function mountRoute(host, kind, first, second) {
  if (kind === 'workflows') {
    const [runtime] = await Promise.all([import('./workflows.js'), import('minarbot-workflows-css')]);
    await runtime.default();
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
