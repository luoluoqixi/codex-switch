# DreamSkin renderer attribution

`renderer-inject.js` and `dream-skin.css` are the generated Windows renderer
assets from Codex Dream Skin v1.5.21, copyright (c) 2026 Codex Dream Skin Studio
contributors, used under the MIT license in `LICENSE`.

Upstream: https://github.com/Fei-Away/Codex-Dream-Skin

Only the DOM renderer and its stylesheet are included. Codex Switch uses Rust
for theme loading, ZIP extraction, preferences and CDP transport. It does not
include DreamSkin's import validator, launcher, Node runtime, theme gallery,
updater or bundled theme artwork.

These files contain Codex DOM compatibility selectors. They may need updating
when upstream Codex changes its interface. Update them together from the same
DreamSkin version; the Windows assets contain compiled selectors and platform
CSS adaptations that are absent from the shared runtime source templates.
