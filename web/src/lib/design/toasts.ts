// Minimal global toast so code outside any component (e.g. the command
// palette's `run()` closures, which live in `commandPalette/items.ts`) can
// surface a message. A single `<Toast>` host subscribed to this store is
// mounted once in the root layout; `toast(message)` is the only API callers
// need.
import { writable } from 'svelte/store';

export interface ToastState {
	message: string;
	color?: string;
}

export const toastState = writable<ToastState | null>(null);

let dismissTimer: ReturnType<typeof setTimeout> | undefined;

/** Show a global toast, replacing any currently shown one. Auto-dismisses. */
export function toast(message: string, color?: string): void {
	toastState.set({ message, color });
	clearTimeout(dismissTimer);
	dismissTimer = setTimeout(() => toastState.set(null), 2400);
}
