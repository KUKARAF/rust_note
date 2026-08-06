import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	// Expose PUBLIC_-prefixed env vars on `import.meta.env` so they are
	// statically inlined at build time (client.ts reads PUBLIC_API_BASE_URL,
	// deviceToken.ts reads PUBLIC_APP_MODE). Vite's default prefix is only
	// VITE_ — without this, `import.meta.env.PUBLIC_*` is silently undefined
	// in the built bundle and the Android app build ends up identical to the
	// website build (dead login flow, wrong API origin).
	envPrefix: ['VITE_', 'PUBLIC_'],
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// Static adapter: this app is a pure client that talks to a remote backend
			// over HTTP/WS. It's built once and served identically as plain static files
			// and from inside a Tauri-wrapped Android build, so there's no Node/edge server.
			// See https://svelte.dev/docs/kit/adapters for more information about adapters.
			adapter: adapter({
				pages: 'build',
				assets: 'build',
				fallback: '200.html',
				precompress: false,
				strict: true
			})
		})
	]
});
