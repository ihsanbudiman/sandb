/**
 * Dev-server settings shared by `vite.config.ts` and the bridge transport. The Rust bridge's
 * `SANDB_DEV_PORT` and `SANDB_BRIDGE_PORT` defaults (`src-tauri/src/bin/bridge.rs`) must match.
 */
export const DEV_PORT = 5199;
/** The dev bridge's default address; `VITE_BRIDGE_URL` overrides it. */
export const BRIDGE_URL = 'http://127.0.0.1:58123';
