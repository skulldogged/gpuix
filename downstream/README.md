# Downstream branches

This fork carries the GPUIX changes shared by Slate and Aurelia. Each
application pins its own branch:

- `main` mirrors `remorses/gpuix`.
- `shared` is upstream plus the changes both applications use. It never
  references an application.
- `aurelia` and `slate` add one integration commit each on top of `shared`:
  the application's crates, native elements and runtime methods. Rebase them
  when `shared` moves.

## Preparing a checkout

The Zed submodule stays on upstream's pin. Its GPUI patches live in
`downstream/zed` and are applied to the working tree:

```sh
bun vendor/gpuix/downstream/prepare.ts
```

## What `shared` adds

GPUI (`downstream/zed`):

- Eased mouse-wheel scrolling in lists and scroll containers.
- Nested scroll containers keep the wheel to themselves.
- Tail-following lists resume following after a layout change.
- Even-pixel relative line heights, so single-line text is centred.
- Shaped text kept across frames, and an unstable scene sort.
- Windows: callbacks unregistered before a closed window is destroyed.

GPUIX:

- `hover` colors and opacity ease in and out, for custom elements too, and
  motion animates `backgroundColor`, `borderColor` and `color`.
- `dragOver` style while OS files are dragged over an element.
- `<gpuix-cache>` regions replayed from the previous frame while unchanged.
- Per-frame work cut: tree walks only when the tree changes, borrowed style
  refinements, painted bounds without tracker children, and mimalloc on
  Windows.
- Markdown: whole-block and streamed-text fades, images, block layout, code
  wrap control, inline code font, and a JavaScript highlight provider
  (`onHighlight`, `answerHighlight`).
- Text asks for its own cursor; `<input secure>` for secrets; anchored
  layers centred on their trigger; navigation mouse buttons.
- Window chrome: `clientDecorations` and `<gpuix-caption action>` for native
  dragging, Snap and caption buttons in an app-drawn title bar;
  `getWindowState()` for maximized, active and appearance.
- `fonts` loads font files before the window opens.
- `<img>` files and URLs load through a bounded memory (`imageMemoryMb`,
  default 256) that drops the least recently drawn images.
- The native crate is its own Cargo workspace, so it builds inside another
  repository.
