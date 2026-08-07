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
	// @excalidraw/excalidraw 0.18 ships modules using arbitrary module
	// namespace identifiers (string-named exports), an es2022 feature — the
	// default build target rejects them. Raise both the production build and
	// the dev-server dependency pre-bundling to es2022. (Vite 8's optimizer
	// is rolldown-based, so the pre-bundle target lives under
	// `rolldownOptions.transform`, not the old `esbuildOptions`.)
	build: { target: 'es2022' },
	optimizeDeps: { rolldownOptions: { transform: { target: 'es2022' } } },
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
