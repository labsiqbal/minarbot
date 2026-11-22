# Interactive replies

Ask an agent to show data as a chart, draw a diagram, or build an interactive comparison. The result appears above the written reply and stays with the conversation. It follows the app's theme and fits its contents as you expand sections or change the window size.

A reply can draft a follow-up in the composer. Review it and send when ready. A reply can also attach context, such as the filters you selected. Use **Remove context** to clear it before sending. External links open in a separate tab.

## Authoring replies

Use [native component replies](component-replies.md) for standard charts, tables, metrics, timelines, Mermaid diagrams, and forms that wait for input.

Use `html_preview` before `html_render`. Pass inline UTF-8 HTML, a readable attachment ID from the conversation, or an HTML file path inside the active workspace. Pages are limited to 512,000 bytes. Preview returns a PNG, measured CSS height, and console diagnostics. It requires Chrome or Chromium and bubblewrap on Linux. Publishing works independently of preview.

Use fluid widths and CSS variables such as `--background`, `--foreground`, `--accent`, `--border`, and `--chart-1` through `--chart-6`. Repaint canvas charts on the `minarbot:theme` event. Let content size the document and give charts a fixed height inside it. Include a text equivalent or accessible data table for charts.

Use self-contained data and images. The frame allows these exact CDN script URLs:

- Chart.js 4.4.9 (MIT) - `https://cdn.jsdelivr.net/npm/chart.js@4.4.9/dist/chart.umd.js`
- D3 7.9.0 (ISC) - `https://cdn.jsdelivr.net/npm/d3@7.9.0/dist/d3.min.js`
- Mermaid 10.9.5 (MIT) - `https://cdn.jsdelivr.net/npm/mermaid@10.9.5/dist/mermaid.min.js`

Call `sendMessage(text)` from a user interaction to draft text. Use `setContext(json)` for up to 4 KiB of JSON reference data. The next send carries the latest valid context. Use local form handlers to update your page. The iframe blocks network forms, API requests, and access to session cookies.

Adapted from T3 Code's visual replies under the MIT license. See [NOTICE](../t3/NOTICE) and [provenance](../t3/provenance.json).
