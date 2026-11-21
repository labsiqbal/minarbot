# Claude Code workspace approvals (#38)

All six tasks that stopped for approval on 0.15.6 passed on branch `t3/38-claude-unattended` with zero interventions. Both runs used Claude Code 2.1.328, device sign-in, and the same benchmark suite hash. One attempt per task. [Claude 0.15.6](0.15.6/2026-11-21T18-33-32.717Z/claude.md) · [Codex 0.15.6](0.15.6/2026-11-21T18-33-32.717Z/codex.md) · [Claude branch run](0.16.0/2026-11-21T20-55-15.989Z/claude.md)

| Six affected tasks | Passed | Interventions |
| --- | ---: | ---: |
| Claude 0.15.6 | 0/6 | 6 |
| Codex 0.15.6 | 6/6 | 0 |
| Claude branch run at `e479c19` | 6/6 | 0 |

The published baseline counts approvals but omits tool names and arguments. A diagnostic replay with a pre-change 0.16.0 debug binary reproduced three stops. Native Bash requested a file-inspection loop for `handover-doc` and `ls` for `paginate-feature` and `slug-feature`.

On Linux, the Claude permission callbacks direct ordinary Bash calls to Minarbot's workspace shell. Explicit host Bash continues through approval. The fake-process regression covers both permission callbacks, executes the checks in the workspace sandbox, and checks isolation from a host-only file, the MCP token, and the host loopback endpoint.

```sh
node bench/run.mjs --runtime claude --tasks cart-bug,expiry-bug,handover-doc,paginate-feature,sales-csv,slug-feature
```
