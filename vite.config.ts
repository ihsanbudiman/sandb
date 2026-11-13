import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import { DEV_PORT } from './src/lib/api/devConfig.ts';

// `SANDB_DEV_PORT` also sets the origin the Rust dev bridge answers for (`bridge.rs`).
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  server: { port: Number(process.env.SANDB_DEV_PORT) || DEV_PORT, strictPort: true },
});
