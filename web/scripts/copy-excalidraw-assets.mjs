// Copy the Excalidraw font assets into `static/` at prepare time.
//
// @excalidraw/excalidraw@0.18 loads its fonts (Excalifont, Virgil, Cascadia,
// CJK fallbacks, …) at runtime from `window.EXCALIDRAW_ASSET_PATH` instead of
// bundling them — ExcalidrawEditor.svelte points that at
// `/excalidraw-assets/`, so the font files must exist under
// `static/excalidraw-assets/fonts/` in every build. They are ~10 MB of binary
// woff2 that would only rot in git, so they are copied from node_modules on
// `npm run prepare` (i.e. after every install) rather than committed — the
// whole `static/excalidraw-assets/` directory is gitignored at the repo root.
//
// Plain node, no deps: `prepare` runs on fresh clones before anything else is
// guaranteed to be installed.

import { cp, access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const webRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const source = path.join(
	webRoot,
	'node_modules',
	'@excalidraw',
	'excalidraw',
	'dist',
	'prod',
	'fonts'
);
const target = path.join(webRoot, 'static', 'excalidraw-assets', 'fonts');

try {
	await access(source);
} catch {
	console.error(
		`copy-excalidraw-assets: source not found: ${source}\n` +
			'Is @excalidraw/excalidraw installed? Run `npm install` in web/ first.'
	);
	process.exit(1);
}

// `cp` with recursive+force mkdir-p's the target and overwrites stale copies,
// so version bumps of the package propagate on the next install.
await cp(source, target, { recursive: true, force: true });
console.log(`copy-excalidraw-assets: copied fonts -> ${path.relative(webRoot, target)}`);
