import { asking } from './ask.svelte';

/** The dialogs App hosts itself, outside the ask host. */
export const overlays = $state({ connection: false, palette: false });

/** A dialog is up: global shortcuts and the native close-tab stay out of its way. */
export const overlayOpen = () => asking() || overlays.connection || overlays.palette;
