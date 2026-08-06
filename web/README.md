# Photonic Zero — browser demo

A static demo of the protocol: one screen transmits, another device's camera
receives. Deployed at the project's Vercel URL.

## No backend, by design

There is nothing to run and nothing to store. The file is read into an
`ArrayBuffer`, encoded by `lib/pz.wasm`, and painted to a canvas; the receiving
device decodes camera frames the same way. Nothing is uploaded, because
uploading would defeat the point of an air-gap protocol.

The recovered payload lives in an object URL that is revoked when the tab is
hidden, when another transfer starts, and on navigation.

## Layout

| Path | What it is |
|---|---|
| `index.html` | The page |
| `style.css` | Single-hue monochrome theme |
| `app.mjs` | Transmit and receive logic |
| `lib/index.mjs` | Copy of the `photonic-zero` npm entry point |
| `lib/pz.wasm` | The compiled module |
| `vercel.json` | Content types, caching, CSP |

`lib/` is committed rather than generated. Vercel builds from git and has no
Rust toolchain, so the module has to be a checked-in artifact.

## Updating the module

After changing anything under `crates/`:

```console
$ cd packages/js && npm run build && npm test
$ cd ../.. && cp packages/js/src/index.mjs packages/js/src/pz.wasm web/lib/
```

## Running it locally

Any static server works, but it must serve `.wasm` as `application/wasm` — or
rely on the loader's fallback, which re-fetches and compiles from a buffer when
streaming compilation rejects the MIME type.

```console
$ npx serve web
```

Receiving needs a secure context, so `getUserMedia` will refuse over plain HTTP
from another device. Use the deployed HTTPS URL to test phone-to-laptop.
