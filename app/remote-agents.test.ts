import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { restoreRemoteMessage } from './src/remote-message-command.ts';

// The runtime cases moved to crates/minarbot-e2e/tests/e2e/pr_remote_agents.rs (#89).
// This case tests the TypeScript module src/remote-message-command.ts.
test('remote message recovery keeps exact target, ID and payload and rejects corrupt storage',()=>{
  const machine=randomUUID(),agent=randomUUID(),command={id:randomUUID(),machine,agent,text:'Keep this request.'};
  assert.equal(restoreRemoteMessage(null,machine,agent),null);
  assert.deepEqual(restoreRemoteMessage(JSON.stringify(command),machine,agent),command);
  for(const raw of ['{bad','null','[]',JSON.stringify({...command,machine:randomUUID()}),JSON.stringify({...command,agent:randomUUID()}),JSON.stringify({...command,id:'bad'}),JSON.stringify({...command,text:' '.repeat(16)}),JSON.stringify({...command,text:'x'.repeat(16_001)}),JSON.stringify({...command,extra:true})])assert.throws(()=>restoreRemoteMessage(raw,machine,agent));
});
