# SyncBlaze Desktop

A small native companion app (built with [Tauri v2](https://v2.tauri.app)) that solves one specific problem the web app can't solve on its own: **a laptop has no camera, and sometimes there's genuinely zero internet anywhere.**

It does two things:

1. Shows the exact same SyncBlaze web app you already know (it wraps the `frontend` repo's build — nothing is duplicated or forked here).
2. Runs a tiny local relay server on your machine's own network, so a phone can reach it directly over Wi-Fi/hotspot and pair with a single QR scan — no camera needed on this computer, no internet needed at all.

It never replaces the web app or the "Quick Connect" cloud-assisted mode — it's the third option, for when neither of those fit.

## How it works, briefly

- On launch, this app starts a WebSocket relay bound to `0.0.0.0:47811` (see `src-tauri/src/relay.rs`).
- The relay only ever forwards opaque JSON between whoever connects with the same pairing code — it has no idea what WebRTC is. The actual peer connection, file chunking, etc. all happens in the same web frontend code that already runs in the browser.
- The phone scans a QR code encoding `ws://<this-machine's-LAN-IP>:47811/pair/<code>` and connects with a normal browser WebSocket — no install needed on the phone side.
- Closing the window hides it to the system tray rather than quitting, so the relay keeps running. Quit for real from the tray menu.

## Repo layout

This is a **sibling repo** to `../frontend` and `../backend`, not a fork of either. `src-tauri/tauri.conf.json`'s `build.frontendDist` points at `../frontend/dist` — Tauri resolves these paths relative to the project root (this `desktop/` folder, not `src-tauri/`), so one `../` is correct to reach the sibling `frontend/` repo. So:

- You need `../frontend` checked out and `bun install`ed before building this.
- There's no root repo tying the three together (matching how `frontend`/`backend` are already split) — CI checks out both explicitly (see `.github/workflows/release.yml`).

## Prerequisites (one-time setup)

- **Rust**, via [rustup](https://rustup.rs). On Windows, the MSVC toolchain must be the default (`rustup default stable-msvc`).
- **Windows only**: [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) — install with the "Desktop development with C++" workload. This is the step people miss; the Rust linker needs it.
- **Windows only**: WebView2 runtime — already present on Windows 11 and modern Windows 10; only very old/locked-down images need it installed separately.
- **Linux only**: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf` (see the CI workflow for the exact apt install).
- [Bun](https://bun.sh) (already required for `frontend`/`backend`).
- The [Tauri CLI](https://v2.tauri.app/reference/cli/): `cargo install tauri-cli --version "^2.0.0"`

## First run

```sh
# from this directory
cargo tauri icon path/to/a-1024x1024-source-icon.png   # generates every platform's icon into src-tauri/icons/
cargo tauri dev                                          # runs the frontend's dev server + opens the app
```

`tauri.conf.json` is already wired to run `bun run --cwd ../frontend dev` for you — you don't need a second terminal for the frontend.

## Building real installers

```sh
cargo tauri build
```

Output lands in `src-tauri/target/release/bundle/`. Unsigned builds work fine locally; see **Code signing** below before distributing them publicly.

## Publishing releases automatically

`.github/workflows/release.yml` builds Windows/macOS/Linux installers and drafts a GitHub Release whenever you push a tag like `v0.1.0`. Before it can run:

1. Set the repository variable `FRONTEND_REPO` (Settings → Secrets and variables → Actions → Variables) to `<your-github-username>/<frontend-repo-name>` — the workflow checks that repo out as a sibling directory, matching the local layout.
2. Push a tag: `git tag v0.1.0 && git push origin v0.1.0`.
3. The draft release appears under this repo's Releases tab — review and publish it manually the first few times.

Once a release is published, point the frontend's `VITE_DESKTOP_DOWNLOAD_URL` (Vercel env var) at this repo's `/releases/latest` URL so the in-app "Download desktop app" buttons go live.

## Code signing (do this before telling real users to download it)

Unsigned installers work, but trigger OS warnings ("Unknown publisher" / "unidentified developer"). This is a real cost/process, not a checkbox:

- **Windows**: OV code-signing certificates can no longer be issued as plain exportable files (2023 CA policy change) — they live on a hardware token or a cloud signing service. [Azure Trusted Signing](https://learn.microsoft.com/azure/trusted-signing/) is the common current path for CI-based signing.
- **macOS**: requires a paid [Apple Developer Program](https://developer.apple.com/programs/) membership ($99/yr), a Developer ID Application certificate, and separately **notarization** (submitting the build to Apple's automated scan). A free Apple ID is not enough — notarization needs the paid membership. Without it, the app shows as "unverified developer" indefinitely, not just once.
- **Linux**: no equivalent gatekeeping outside package-manager ecosystems.

## Google sign-in

Google's popup-based sign-in doesn't work inside an embedded webview (WebView2 blocks the cross-origin storage it needs, and Google generally distrusts embedded webviews for auth). So this app opens the real system browser instead, via [PKCE](https://datatracker.ietf.org/doc/html/rfc7636) — no client secret is stored or transmitted anywhere; see `src-tauri/src/oauth.rs`.

This needs its own **separate** Google OAuth client (type: **Desktop app**, not Web application) — create one at [console.cloud.google.com/apis/credentials](https://console.cloud.google.com/apis/credentials) in the same project as the website's existing client. Only the Client ID matters (not the secret Google also generates — it's unused by this flow).

To test locally: add `VITE_GOOGLE_DESKTOP_CLIENT_ID=<that client id>` to `../frontend/.env`, and `GOOGLE_DESKTOP_CLIENT_ID=<same id>` to `../backend/.env`, then restart both `cargo tauri dev` and the backend.

For release builds: set the `VITE_GOOGLE_DESKTOP_CLIENT_ID` repository variable here (same place as `VITE_API_URL` etc.), and `GOOGLE_DESKTOP_CLIENT_ID` on the backend's production host (Render).

## Known limitation worth knowing about

`local_ip_address::local_ip()` picks *a* local network interface automatically — on a machine with several active interfaces at once (Wi-Fi + Ethernet + a VPN adapter, say), it doesn't always guess the one the phone is actually on. If pairing fails on a machine like that, the fix is checking the actual IP shown in the app against `ipconfig`/`ifconfig` output — a manual interface picker is a reasonable follow-up if this turns out to matter in practice.
