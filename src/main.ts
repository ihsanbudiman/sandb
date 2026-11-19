import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';
import { loadConnections } from './lib/connections.svelte';
import { loadAiSettings } from './lib/state.svelte';
import { loadAppearance } from './lib/appearance.svelte';
import { startScheduler } from './lib/schedules.svelte';

loadAppearance();
loadConnections();
loadAiSettings();
startScheduler();

export default mount(App, { target: document.getElementById('app')! });
