# React and Leptos screenshot parity

Build the React app and Leptos route bundles. Install the smoke package with
`npm ci --prefix test-support/ui-smoke` from `app/` and install its Playwright
Chromium. Run from `app/` with the workspace server built.

```sh
~/.cache/minarbot-node-slot.sh npm run parity:gate -- /tmp/minarbot-parity
```

Set `PARITY_GROUPS` to a comma-separated list of group names (for example
`PARITY_GROUPS=calendar,automation`) to run only those groups. The release
review runs all groups.

Pass an empty output directory or omit it to use a fresh temporary directory.
`MINARBOT_RUST_BINARY` can point at a different build of the server. The gate
uses disposable servers and captures at least 580 pairs across 16 groups. Its
source smokes still run their functional assertions. Each group has a log, and
`pairs/` contains side-by-sides with React on the left, TSVs for all pairs,
nonidentical pairs, and failures, and per-group totals in `summary.json`.

Each pair must have the same dimensions. A pixel counts as changed if any RGB
channel differs. At most 100 pixels may change, and at most 24 may have a
channel delta greater than 16. In the previous review, the settled conversation
pairs peaked at 61 changed pixels and three over 16. These limits allow small
raster differences while catching visible changes. Missing captures and failed
smokes also fail the gate. The bounding box records changed pixels with an
exclusive right and bottom edge.

The Projects smoke also exercises Leptos-only project messaging and archive
restoration. Conversation controls exercise Leptos-only search across chats.
These produce diagnostic captures alongside the paired screens.

Narrow Hub keeps its top navigation so content and composer can use the full
viewport. The gate includes room, members, invite, invitation, and create project
in both themes.
