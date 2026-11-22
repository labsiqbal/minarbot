# Native component replies

Use `render_component` for bounded, accessible UI that fits a conversation. The payload has a `tree` with a stack of catalog nodes, a `data` object keyed by each node, and `actions` with explicit context. Supported nodes are `bar`, `line`, `pie`, `table`, `metric`, `timeline`, `mermaid`, and `form`.

Charts render as SVG with a **View data** table. Tables use captions, column headers and a keyboard-scrollable region. Mermaid runs in the sandbox frame used by `html_render`, with the same CSP and pinned Mermaid 10.9.5 asset. Use `html_render` for custom layouts or libraries.

Forms accept text, numbers, checkboxes and selections. A form holds the tool call until the user submits, skips, stops the turn or reaches the 15-minute limit. The result contains `status`, `action`, `context` and typed `values`. Never request credentials or secrets in a form.

The component tree and data model are schema-validated in Rust before storage. The React renderer is the shipped client. The same payload is exposed through Minarbot MCP, where `render_component` holds the tool call open while the form is pending.

Ideas for explicit component trees, typed actions and tool-backed generative UI were informed by [CopilotKit](https://github.com/CopilotKit/CopilotKit), MIT licensed. Minarbot implements its own renderers and contracts.
