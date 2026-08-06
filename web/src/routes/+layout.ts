// This app is a pure client-side SPA (adapter-static): it is served as static
// files both from a plain web deployment and from inside a Tauri-wrapped
// Android build, and talks to a remote Rust backend over HTTP/WS. There is no
// SSR and no Node/edge server, so we prerender what we can and disable SSR
// entirely for everything else (dynamic routes fall back to index.html and
// are resolved client-side).
export const prerender = true;
export const ssr = false;
