import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import { DEV_PORT } from './src/lib/api/devConfig.ts';

// `SANDB_DEV_PORT` also sets the origin the Rust dev bridge answers for (`bridge.rs`).
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  // Discover lazy chart/PDF dependencies before the first render or export can trigger a dev reload.
  optimizeDeps: {
    include: ['echarts/core', 'echarts/charts', 'echarts/components', 'echarts/renderers', 'jspdf', 'svg2pdf.js'],
  },
  server: { port: Number(process.env.SANDB_DEV_PORT) || DEV_PORT, strictPort: true },
});
