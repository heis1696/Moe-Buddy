# Moe Buddy Tauri desktop host

Windows-first Tauri 2 host for the Vue frontend. Both `main` and `pet` load the same local `index.html`; the frontend selects its view from the Tauri window label. The `moe-chat-owner` thread directly owns one `BuddyStorage` for the process lifetime, which holds the shared LocalMemory/root locks and every opened buddy `ChatApplication`; the host keeps per-buddy process-only credentials and request receivers beside it. The same worker owns `Animator`. Commands cross a 32-item bounded queue, close/confirm commands cross a separate 8-item queue, and command payloads are capped at 32 KiB.

## Host coordination

`prototype.rs` keeps the business state, buddy selection, close confirmation, exit intent, and controlled window effects in one `HostWorker`. IPC DTOs are defined in `src-tauri/src/contract.rs`; `src/core/contract-fixture.json` remains the shared frontend/Rust contract. Every `prototype_dispatch` command and `prototype_management_draft_state` update carries the captured `{ expectedBuddyId, expectedBuddiesRevision }`. The worker revalidates it when consuming normal commands, priority close messages, management state, and queued main-window visibility actions. A missing or stale context is rejected before business state changes.

- `show_chat`/`hide_chat` carry the host snapshot context, apply the business `Open`/`Hide` first and only then change the physical window. A revoked panel only permits physical visibility changes because there is no business panel left to open.
- Closing with a chat draft creates the core close confirmation. Its `confirm_close` command requires `expectedIntent` (the current `closeIntent`, including explicit `null`) and the captured buddy context. Management state also carries required `kind: none | general | buddy_create | buddy_rename`; its `{ version, dirty }` acknowledgement stays body-free. Create/rename consumption requires the exact management version, kind, buddy, and registry revision. A new edit expires the old confirmation, cancel preserves edits, and an accepted discard is echoed by version before the host continues.
- Exit requests use a separate latest-intent mailbox, so a full 32-item normal queue cannot drop them. Before a queued confirmation is decided, the worker applies already accepted normal updates, merges accepted exit requests, and rechecks the bound version and intent. A stale confirmation keeps the draft and presents the upgraded intent; exit still requires a matching confirmation. Cancel only clears an intent after its own binding is validated.
- The mailbox keeps the newest request ID while merging intent by precedence, so a later close cannot downgrade an unconsumed quit. Confirmation snapshots expose `closeIntent` (`close_chat` or `quit_application`) and advance `presentationRevision` when host intent changes without a business revision change.
- Error envelopes contain a safe `public_message` and `disposition` (`not_executed`, `settled`, or `unknown`) projected from `AppError`; model-settings persistence also retains its typed I/O stage internally. The frontend does not infer execution from error text. IPC reply loss and explicit business Unknown are tracked separately, and neither is cleared by an ordinary snapshot read without matching completion evidence.
- New business commands still enter through `prototype_dispatch` and are accepted only from `main`. They map to existing `ChatApplication` actions for panels, presets, memory CRUD/search, and diagnostics; `create_buddy`, `switch_buddy`, and `rename_buddy` use `BuddyStorage` as the only profile authority. The Tauri DTO includes a required `apiKey` string or null for model configuration. Session lifecycle adds `new_session { expectedSessionId, expectedSessionsRevision }`, `switch_session { id, expectedSessionId, expectedSessionsRevision }`, and `archive_session` with the same expected fields; queued `set_draft` and `send` also require both expected fields.
- Snapshots include `activeBuddyId`, `buddiesRevision`, bounded body-free `buddies` profile summaries, and only the active trusted `activeBuddyPersona`. `buddiesRevision` advances on creation, switch, rename, and configuration commits. Each buddy app stays open in `BuddyStorage`, so A→B→A retains that app's session drafts, memory identity, diagnostics, and runtime watermarks. The global `presentationRevision` is assigned by the worker and never resets to a buddy's local application revision.
- Snapshots require `buddyCommit`, null at startup and after revocation or a failed operation. A successful record contains only `{ kind, fromBuddyId, fromRevision, toBuddyId, toRevision, managementVersion }`; `toRevision` is the committed registry revision. Create/rename use the exact submitted management version; switch/model/preset commits capture the accepted management version. The host publishes it only after the registry and required live runtime binding commit. Model/preset saves coordinate the accepted management mailbox state to the new context while retaining its dirty/kind/version values; queued messages carrying the old context remain stale. The receipt is memory-only and does not clear API key input or imply that any unrelated Unknown is resolved.
- A registry `Unknown` freezes selection and ordinary business dispatch. A captured private external ticket can still settle only its captured, already-loaded buddy; the host can inspect that known instance, read/preflight/discard the exact full-buddy draft watermark set after explicit Quit confirmation, revoke that known instance, and hide an already-open non-chat management panel under the captured active-buddy context. Presentation hide/close never reports success if the native window operation fails. None of these paths opens a new instance or returns general mutable application access. Quit remains two-stage when needed: cancelling either management or chat confirmation preserves the corresponding drafts; final confirmation consumes only the previously checked revisions.
- Snapshots include `activeSessionId`, `sessionsRevision`, and a body-free `sessions` directory with `id`, `label`, `archived`, and `status`. Revocation returns a null active ID and an empty directory. Draft text stays in the trusted application and is never included in a directory entry.
- Snapshots also include the body-free `chatDraft` projection: `{ revision, discard, sessions }`. `revision` is the application's current safe-integer draft-set revision; `discard` is either `null` or `{ revision, sessionIds }`, the authoritative receipt of one explicit `CloseChat`/Quit discard. `sessionIds` lists the sessions whose unsent text that discard consumed (sorted, deduplicated, at most 64 IDs). `sessions` carries at most 64 `{ id, revision, discardedRevision }` entries: each live session's latest draft-change revision and latest explicit discard revision (`null` when never discarded). No draft body, queue, ticket or execution right is exposed. Ordinary empty snapshots, hiding/reopening, status re-reads and normal edits never create a receipt; a later edit only moves the session/global revision past the receipt so it becomes historical. Revocation returns an empty session list and a null discard.
- Persistent lifecycle commands use each buddy's own ChatApplication and per-buddy RawLocalStore under the one root lock and budget. The worker calls `maintain_retention` before checkpoints and on a bounded 30-second cadence. Recycle-zone backpressure is surfaced as a safe notice; maintenance failure leaves writes closed.
- `configure_model` publishes persona, model endpoint, model name, preset ID, and profile/model revisions in one `buddies.v1` commit, then applies the user's explicit transmission choice to that same version. It never configures the app with consent disabled as a preliminary commit. API keys and consent remain in per-buddy process memory only. Null preserves that buddy's key only when endpoint/model are unchanged; an empty string clears it. After restart and for a new buddy, both key and consent are absent. Endpoints use the full `/v1/chat/completions` path.
- `Send` is rejected before transport when transmission is not permitted. The transport receives immutable input and the captured buddy's credential on its worker thread; the owner thread keeps `{ buddyId, ExternalRequestTicket, receiver }` together and returns each callback only to that cached app. A rejected ticket cannot consume another app's pending request. Disconnects stay `unknown` and are not retried. Revocation clears only the active buddy's credential and public settings; late results remain bound to their original ticket and app.
- Quit captures the full body-free draft revision set for every loaded buddy plus the management version/context. The host preflights every app and the whole set before consuming any chat draft, then discards each app's drafts only at the matching revisions. A new edit invalidates the confirmation. CloseChat remains scoped to the active buddy's current session.
- Snapshot payloads include the bounded preset, memory, diagnostic, notice, model-setting, and persistence projections from the frozen D contract. The snapshot event targets `main` only; pet commands cannot read a snapshot or dispatch business actions.
- Matching `managementDiscardedVersion` receipts are consumed at the shared snapshot/response entry point, so an event can clear a confirmed local draft even if its invoke response is lost. Old receipts cannot clear later edits or unlock unrelated Unknown command locks. Management callbacks also recheck component lifetime, revocation, active panel, and interaction generation before writing back.
- `chatDraft` discards are consumed at the same shared entry, per session instead of per global revision. Each cache entry records the draft-set revision it synced and the local edit generation, and `chatDraft.sessions` supplies one consumed watermark per session. An edit of another session (which advances the global revision) can no longer make A's older receipt look stale, and a host that missed intermediate discard events still clears every session whose latest per-session watermark moved. A cached body is removed only when its synced revision is older than that session's discard watermark; a newer confirmed edit, a newer unsynced body, and other sessions' drafts are preserved. The current composer is cleared only when its own session was discarded and the local queue is synced, before any cache-restoring entry runs; an event-first or response-first delivery, a re-read after a lost reply, a duplicate response, an old queued reply, or a reopened window therefore cannot resurrect the discarded text or auto-dispatch `set_draft`. The receipt never unlocks an Unknown command, and revocation clears the cache and watermarks before any late receipt can write back.
- The host opens the default `%LOCALAPPDATA%/MoeBuddy/daily` root. `--data-dir <path>` selects a root at process startup; the WebView cannot pass a path. Restore errors abort startup with a nonzero exit instead of creating an empty replacement archive.
- Before the event loop stops, the worker stops the animator, releases the application object, marks itself finished, and only then requests `AppHandle::exit(0)`. If an external exit request arrives first, the run-event callback prevents it, signals the worker, and lets the worker request the exit after release.
- Tray visibility/pause/quit failures set a fixed safe tray tooltip and stderr diagnostic instead of being swallowed. Animation progress comes from `Animator::playback_progress`; frame snapshots carry paused/faulted state. Canvas draw failures are shown independently and cleared after a successful draw; emitting a frame event does not attest that rendering succeeded.

## Portable Windows test package

From the workspace root, run `powershell -NoProfile -File scripts/package-windows.ps1` after installing the locked frontend dependencies. It builds the actual Vue assets and an offline locked Rust release, then creates a portable directory, ZIP and SHA-256 sidecar under ignored `releases/`. Windows x64 and the WebView2 Evergreen Runtime are required; Node and Rust are only build requirements. `-SkipBuild` repackages an already verified release binary and records that fact in BUILD-INFO.json.

Extract the whole ZIP into a writable directory and double-click **Moe Buddy.exe**. The adjacent `moe-buddy.portable` marker must contain exactly `1` followed by LF. It selects executable-relative data and data/webview; malformed markers refuse startup rather than silently selecting daily storage. Explicit `--data-dir` overrides portable selection and retains the existing developer profile behavior. No user archive, API key, consent or private artwork is packaged. Release startup errors also show a native message, including the possible already-running case; existing storage locks remain authoritative.

The main header's **退出应用** button flushes pending management/chat edits, rechecks the captured buddy scope, then uses the same guarded Quit path as the tray. Revoked contexts may still exit. All-buddy draft confirmations remain enforced; a lost reply is not automatically retried. Window X retains CloseChat behavior. `MOE_SMOKE_BINARY` can select an extracted release executable for the existing real-window smoke suite; synthetic data/profile roots remain explicit.

`node apps/moe-buddy-tauri/tests/portable-smoke.mjs <extracted-executable>` checks a fresh package extracted under target, with no data/profile arguments, draft cancel/confirm and same-identity restart. It refuses a package that already has data. The ordinary native smoke keeps explicit temporary roots and exercises the new Exit button, except the deliberate Vue-unmount fixture which uses host IPC for cleanup. Both suites require an idle keyboard/mouse; system input must not modify synthetic fields during assertions.

## Material package API

The Tauri host's `material` module accepts a local package directory with a UTF-8 `manifest.json`. A manifest has `formatVersion: 1`, a package `id`, positive safe-integer `version`, `label`, `clips`, `actions`, and `defaultAction`. A sequence clip contains ordered `{ path, durationMs }` PNG frames; a GIF clip names one GIF file whose frame delays are used as encoded. Each action selects a clip and declares `mode: "once" | "loop"`. Paths are package-relative slash paths. Archives, URLs, scripts, SVG and APNG are not package inputs.

The initial PNG decoder supports static 8-bit non-interlaced grayscale, RGB, indexed, grayscale-alpha and RGBA images. Adam7, other bit depths and APNG are rejected as unsupported. GIF frames are composited on a transparent canvas: disposal 0/1 retains pixels, disposal 2 clears the preceding frame rectangle to transparent RGBA zero, and disposal 3 restores the canvas from before that frame. Transparent palette indices preserve existing pixels; opaque entries use alpha 255. GIF delays are not browser-clamped.

Rust callers load immutable packages with `MaterialPack::load(directory)` and create an independent `SequenceClipPlayer::new(pack)` for the existing `moe_agent_runtime::animation::Animator`. The player exposes borrowed current-frame views and a separate `SequenceFrameReader` handle containing only immutable frame metadata and shared pixel bytes; it does not implement `ClipPlayer` or retain a playback instance. The host prepares a replacement before stopping the active channel, then starts it only after stop succeeds. `stop` releases the active ticket but keeps the validated package and Animator so a later explicit `play` can create a fresh ticket without rereading files. Package loading rejects malformed data and enforces manifest, encoded-byte, decoded-pixel, frame-count, dimension and duration limits before publishing a package.

The Tauri host exposes `prototype_material_state`, `prototype_material_command`, and `prototype_material_pixels`. State and `prototype://material` events share the same path-free projection and are sent only to main and pet windows. Main-window commands carry the complete buddy context and both material generations; loading uses one cancellable worker and bounded result channel, then rechecks context and generations before publication. Pet pixels are copied only for the current package frame triple, and native hit masks are checked against that same triple at application time. Stop releases the active ticket while retaining a package for replay; unload, builtin restore, replacement, and host shutdown release package pixels after confirmed stop. Package paths remain process-only and do not affect buddy identity, profiles, memory, sessions, credentials or send permission.

### Loading your own material

Create a local folder containing `manifest.json` and your image files. Drag the folder (or its `manifest.json`) into the main window's material area. The following package uses two static PNGs and one GIF:

```json
{
  "formatVersion": 1,
  "id": "my-companion",
  "version": 1,
  "label": "My companion",
  "clips": [
    { "id": "idle", "type": "sequence", "frames": [
      { "path": "frames/idle-0.png", "durationMs": 150 },
      { "path": "frames/idle-1.png", "durationMs": 250 }
    ] },
    { "id": "wave", "type": "gif", "path": "wave.gif" }
  ],
  "actions": [
    { "id": "idle", "label": "Idle", "clip": "idle", "mode": "loop" },
    { "id": "wave", "label": "Wave", "clip": "wave", "mode": "once" }
  ],
  "defaultAction": "idle"
}
```

Every frame must have a positive duration. GIF timing comes from its own frame delays; its embedded loop count does not override the action mode. Images and GIF logical canvases are limited to 512×512, with at most 256 frames per clip, 512 per package, and 32 MiB of decoded pixels. The manifest is limited to 64 KiB, individual encoded files to 8 MiB, and total encoded reads to 16 MiB. Unknown fields, unsupported formats, and invalid paths are rejected while the current valid package keeps playing.

Select an action and use Play, Pause, Resume, Stop, or Unload. Stop retains the decoded package for replay; Unload releases it. Restore builtin returns to the test cat. Package selection lasts for the current process and does not change the companion's identity or private history. Windows loading retains read-only directory handles (up to 128 path components per chain) through validation and holds each image against writes or replacement during its read; concurrent edits may therefore cause a clear loading failure. Image paths stay relative to the package and cannot cross symbolic links or junctions.

## Run

From the workspace root, install the locked frontend dependencies, build the Vue app, and start the host:

```powershell
npm --prefix apps/moe-buddy-tauri ci
npm --prefix apps/moe-buddy-tauri run build
cargo run -p moe-buddy-tauri --locked
```

The default data root is `%LOCALAPPDATA%\MoeBuddy\daily`. To choose an explicit root:

```powershell
cargo run -p moe-buddy-tauri --locked -- --data-dir D:\MoeBuddy\daily
```

Frontend commands (from this application directory):

```powershell
npm ci
npm run dev
npm run build
```

The host can use an explicitly configured HTTPS model endpoint; HTTP is accepted only for numeric loopback addresses used by local services and synthetic tests. It does not load remote frontend content. API keys and transmission permission are process-only and are never saved. Generated frontend and Tauri schema outputs are ignored by Git and are not checked-in source. The host currently loads built assets; the standalone Vite page reports that desktop startup is required and does not simulate IPC.

## Verification

```powershell
npm ci
npm run typecheck
npm run test
npm run build
cargo test -p moe-buddy-tauri --locked
cargo clippy -p moe-buddy-tauri --all-targets --locked -- -D warnings
cargo test -p moe-desktop-adapters --locked --offline
```

The host regression tests drive the production `Request`/`CloseRequest` entries and the worker loop, not `ChatApplication` directly. They cover the existing close/exit coordination and the D entry points for denied transmission, configuration/save failure, loopback completion, disconnect unknown, model-key rules, memory revision conflict, diagnostics bounds, persistence restore, and late-ticket rejection after revocation. Adapter storage and transport tests use synthetic temporary roots and loopback servers.

Rust and TypeScript share the checked field/enumeration fixture in `src/core/contract-fixture.json`. Component lifecycle tests execute the production Vue script with controlled IPC completion to check that released editors and snapshot caches cannot be refilled; these tests do not substitute for DOM validation. The Vue caller has not yet been updated for the new required session guard fields, so session IPC is not yet wired to the visible selector; that frontend change is separate from this Rust installment. Persisted session directories, execution-generation rotation, cold-history loading, role profiles, and production material packs remain later work.

### Native smoke

After building the frontend and the host binary, run the real Tauri/WebView2 smoke:

```powershell
npm run smoke:native
```

`tests/native-smoke.mjs` starts the built `target/debug/moe-buddy-tauri.exe`, gives each process an isolated `--data-dir` and WebView2 profile under `target/native-smoke/`, and fills real Vue forms and clicks buttons using WebView2 CDP input events. The model endpoint is an in-process Node HTTP fixture bound to `127.0.0.1`. Tests cover configuration/consent/key clearing, send/reply display, private memory CRUD/search and recoverable version conflict, diagnostics, restart recovery, dirty-editor close/quit confirmation, and definite versus unknown settings failures. Direct IPC is reserved for source restrictions, readback, and creating an independent concurrent memory edit. Every created process is retained for identity-checked cleanup; a failed process cannot silently retain the archive lock for the next instance. Reports are written under `target/native-smoke/`.

Limitations are recorded in the report: synthetic loopback model only; real IME, mixed DPI, non-Windows targets and clicking the tray popup are unverified. Native close and tray quit use WM_CLOSE and WM_COMMAND. The unload smoke verifies the real button path and authoritative memory result; private released Vue caches are checked separately by the component lifecycle regression, without a production inspection backdoor.

## Native dependencies

The host pins `tauri` 2.12.4 and `tauri-build` 2.7.3. Tauri is isolated to this app. It introduces the platform windowing, WebView, tray, and build-time resource dependency tree; the 1.99-compatible lock currently resolves roughly 300 additional crates. Existing `serde` 1.0.229, `getrandom` 0.4.3, and `windows-sys` 0.61.2 versions are reused. `windows-sys` stays in the host adapter for alpha hit regions, cursor polling, DPI-scaled client coordinates, and click-through style changes.

`prototype://snapshot` is delivered only to `main`; `prototype://frame` updates both local windows without chat content. Window visibility and quit requests capture a buddy context at their initiating host command/event and carry it through the queue or exit mailbox for execution-time validation. The frontend capability grants event listening and unlistening only. The native pet window uses an alpha-derived Windows region; unsupported platforms do not silently substitute whole-window click-through.
