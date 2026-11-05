# alt workbench

The new server workbench uses the versioned `@golia/ui` archive in `vendor/`. The archive contains the shared sentori components, source, type declarations and licenses; its recorded identity is in `vendor/golia-ui.json`. Building does not require a private package registry or another repository checkout.

```sh
bun install --frozen-lockfile
bun run check
bun run build
```

API calls use the same origin and server sessions. Browser credentials are not stored in local storage. Each open document retains its original repository; changing the resource selector does not retarget an existing review. A lost mutation response remains unconfirmed and is never automatically replayed.

This directory is under active server integration. The existing `frontend/` product and repository-browser site remains a separate application until migration is accepted.

The real-browser acceptance script runs against the live dual-client task fixture,
with the production build served by `alt-server` and real PostgreSQL. Install the
matching Playwright Chromium with `node node_modules/playwright-core/cli.js install
chromium`, then provide absolute `ALT_WORKBENCH_ASSETS` and
`ALT_WORKBENCH_ACCEPTANCE` paths to the scoped `task_remote::two_clients_continue`
integration test. `ALT_WORKBENCH_REPORT` selects the output directory for results
and screenshots. This fixture creates isolated accounts and data and does not
connect to a production instance.
