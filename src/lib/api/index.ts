import { makeBackend, type Transport } from './backend';
import type { Backend, Capabilities } from './types';
import type { CommandName } from './generated/wire';
import { tauriTransport } from './tauri';
import { mockTransport } from './mock';

// The bridge client is imported only behind the `import.meta.env.DEV` check, so a production build
// drops it (and any bridge token) entirely.
function bridgeTransport(): Transport {
  let loaded: Promise<Transport> | undefined;
  const transport = () => (loaded ??= import('./http').then((m) => m.httpTransport));
  return {
    async call<T>(name: CommandName, args?: Record<string, unknown> | Uint8Array, headers?: Record<string, string>): Promise<T> {
      return (await transport()).call<T>(name, args, headers);
    },
    async stream(name, args, onEvent) {
      return (await transport()).stream(name, args, onEvent);
    },
  };
}

const desktop: Capabilities = {
  fileExport: true,
  quitPrompt: true,
  unsavedFlag: true,
  nativeMenu: true,
  windowEffects: true,
  pageUnload: false,
  updates: true,
  secretStorage: 'encrypted',
  ai: true,
};

const bridge: Capabilities = {
  fileExport: false,
  quitPrompt: false,
  unsavedFlag: false,
  nativeMenu: false,
  windowEffects: false,
  pageUnload: true,
  updates: false,
  secretStorage: 'memory',
  ai: true,
};

const demo: Capabilities = {
  fileExport: false,
  quitPrompt: false,
  unsavedFlag: false,
  nativeMenu: false,
  windowEffects: false,
  pageUnload: true,
  updates: false,
  secretStorage: 'none',
  ai: false,
};

function pick(): Backend {
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) return makeBackend(tauriTransport, desktop);
  if (import.meta.env.DEV && import.meta.env.VITE_BACKEND === 'bridge') return makeBackend(bridgeTransport(), bridge);
  return makeBackend(mockTransport, demo);
}

export const api: Backend = pick();

export * from './generated/wire';
export * from './types';
