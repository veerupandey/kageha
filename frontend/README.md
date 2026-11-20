# Kageha Web UI

The React frontend uses Vite. See the [project README](../README.md#build-from-source) for backend startup and [Using Kageha](../docs/USAGE.md) for the Web UI workflow.

Run these commands from this directory.

```sh
npm ci
npm run lint
npm test
npm run build
```

For hot reload, start the Rust Web UI server and run `npm run dev`. Set `KAGEHA_WEBUI_BACKEND_URL` to its local address when it differs from the Vite proxy default.
