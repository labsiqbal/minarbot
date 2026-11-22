import { startRustServer } from './rust-test-server.ts';
import { rmSync } from 'node:fs';
import type { Server } from 'node:http';

export async function startApp(options: Parameters<typeof startRustServer>[0], fixtures?: { directory: string; servers?: Server[] }) {
  try {
    return await startRustServer(options);
  } catch (error) {
    if (!fixtures) throw error;
    try {
      await Promise.all((fixtures.servers ?? []).map(server => new Promise<void>(resolve => {
        server.close(() => resolve());
        server.closeAllConnections();
      })));
      rmSync(fixtures.directory, { recursive: true, force: true });
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], 'Rust test server startup and fixture cleanup failed.');
    }
    throw error;
  }
}

export const retainedToolNames = [
  'ask_user', 'call_mcp_tool', 'cancel_task', 'copy_skill_file', 'create_text_file', 'create_workspace_directory',
  'create_workspace_file', 'delegate_task', 'delete_site', 'edit_profile', 'generate_image', 'inspect_workspace_file',
  'interrupt_agent', 'list_agents', 'list_data_tables', 'list_mcp_tools', 'list_routines', 'list_sites', 'list_skills',
  'list_workspace', 'no_update', 'open_file_in_panel', 'publish_site', 'query_data', 'read_attachment', 'read_history',
  'read_message', 'read_provider_settings', 'read_routine', 'read_skill', 'read_skill_file', 'read_workspace_file',
  'remember_memory', 'report_failure', 'search_memory', 'select_memory', 'send_message', 'set_avatar', 'set_routine_flow',
  'task_status', 'transfer_task', 'wait_for_event', 'wait_task',
].sort();

import { canonicalTestBridge, type CanonicalFixtures } from './rust-canonical-test-bridge.ts';
import { freePort } from './test-support/test-process.ts';

type RustServerOptions = Parameters<typeof startRustServer>[0];
type CanonicalOptions = RustServerOptions & CanonicalFixtures & { syncKey?: () => Uint8Array };


/** Starts canonical-mode Rust with serializable Clerk, hub, executor, and sync options. */
export async function startCanonicalApp(options: CanonicalOptions, portRetries = 0): ReturnType<typeof startRustServer> {
  const port = options.port && options.port !== 0 ? options.port : await freePort();
  const configuredOrigin = options.auth?.origin;
  const auth = options.auth && { ...options.auth };
  if (auth && /^http:\/\/127\.0\.0\.1$/.test(auth.origin)) {
    auth.origin = `http://127.0.0.1:${port}`;
    if (options.auth) Object.assign(options.auth, { origin: auth.origin });
  }
  const { clerk, syncKey, routineWebhookKeys, mail, ...serializable } = options;
  const testKeyring = { ...(serializable.testKeyring ?? {}) };
  if (typeof syncKey === 'function') {
    const value = syncKey();
    if (!(value instanceof Uint8Array) || value.length !== 32) throw new Error('Sync key must be 32 bytes.');
    testKeyring['sync-hmac/v1'] = Buffer.from(value).toString('base64url');
  }
  const bridge = await canonicalTestBridge({ clerk, routineWebhookKeys, mail });
  try {
    const app = await startRustServer({ ...serializable, auth, port, testKeyring, testCanonicalBridge: bridge.config });
    return { ...app, close: async () => { try { await app.close(); } finally { await bridge.close(); } } };
  } catch (error) {
    await bridge.close();
    if (!options.port && portRetries < 4 && error instanceof Error && 'code' in error && error.code === 'EADDRINUSE') {
      if (options.auth && configuredOrigin) options.auth.origin = configuredOrigin;
      return startCanonicalApp(options, portRetries + 1);
    }
    throw error;
  }
}
