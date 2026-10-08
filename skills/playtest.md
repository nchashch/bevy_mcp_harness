# Skill: Playtesting a Bevy app with the bevy_mcp_harness

Read this before driving an app as an agent. It is the distilled, battle-tested
playbook for launching a Bevy app headlessly, driving it through the QA tool API,
observing state, capturing what you see, and avoiding every trap hit so far.
Written for **any Bevy 0.19 project** that hosts `BevyMcpHarnessPlugin` (this
repository is the harness; `examples/headless.rs` is the reference fixture).
Supplements (does not replace) this repo's `AGENTS.md`.

Sections marked **[p19]** at the end collect the parts that are specific to
prototype_19 (the app this harness was extracted from): its launch flags, its
game flow, its server, and its documentation-system conventions. Everything else
applies to any Bevy app that hosts the plugin.

**Default observation method: data, not pixels** (§6) — understand the world
via `game/state`/`game/ui`/BRP queries; screenshot only when asked or when the
thing under test is inherently visual. If the data surface is missing what you
need, report the API gap rather than falling back to screenshots.

**Default input method: gamepad** (§4) for UI navigation, because gamepad mocks
are discrete and level-triggered (no pixel coordinates, no timing races) and
they flow through the same binding resolution a human's controller does. Use
keyboard/mouse when the task explicitly targets them.

## 1. The processes

| Process | Binary | Ports |
|---|---|---|
| App under test (headless agent host) | the host binary with `BevyMcpHarnessPlugin` configured | BRP HTTP :15702 · MCP :15710 (both configurable) |
| Game server (if the game is client/server) | the host's server binary — it can host the harness too, on different ports | app-specific |
| You (the agent) | shell + `curl`/python | talks to the app's BRP port |

In this repository the app under test is `cargo run -q --example headless`
(render-less) or `cargo run -q --example headless -- --render` (rendered,
offscreen Vulkan). In a game repo it is whatever binary hosts the plugin.

Render-less hosting (`no_render: true` in `McpHarnessConfig`): everything in
this playbook works **except screenshots** — the render plugins (and the
wgpu/Vulkan instance) are absent, so no GPU driver is needed and CPU cost drops
by an order of magnitude. UI layout, `game/ui`, hover/clicks, input mocking are
identical (`bevy_ui`'s layout/picking is render-free logic; the harness's
`shim_camera_computed` feeds the one camera value UI reads back from the render
side). Default choice for gameplay/UI/logic fleets on small boxes; use a
rendered host when a check is inherently visual (§6).

## 1a. Host configurations — call `game/client_info` first

Every app reports its effective launch configuration via `game/client_info`
(mode flags, surface ports, `screenshots_dir`, `target_size`). **Call it before
anything else on a fresh session** — it tells you which tools are meaningful
here, without trusting whatever launch line someone else used.

| Composition | Configuration | Renders? | Screenshots? | Notes |
|---|---|---|---|---|
| Rendered headless | `offscreen_size: Some(...)` | yes, into a shared offscreen texture | ✓ (crop, unchanged-suppression) | All cameras are retargeted into one texture; agent-cursor crosshair overlay is drawn into captures |
| **Render-less headless** | `no_render: true` (typically with `offscreen_size` too) | **no** (no Vulkan needed at all) | ✗ clean error — use `game/ui` + BRP queries | UI layout, `game/ui`, hover/clicks, input mocking all work; worlds/meshes never render |
| Windowed | neither (default `McpHarnessConfig`) | real window | ✓ primary window | OS cursor is visible (no crosshair overlay); `game/mouse` targets the window's pixel space |
| Fleet member | `--brp-port N --mcp-port N` (via `from_env`) | per above | ✓ (isolated `screenshots/client-N/` dir) | Many instances, one machine (§8) |

Particularities worth remembering:

- CLI flags `--brp-port N` / `--mcp-port N` / `--no-render` are read by
  `McpHarnessConfig::from_env` if the host opts into it. Reported flags in
  `game/client_info` are the **effective** ones — trust them over the launch
  line.
- In render-less mode `screenshots` returns a **descriptive error** — that is
  correct behavior; don't work around it, switch to a rendered host.
- The agent-cursor crosshair is drawn only when an offscreen target exists
  (windowed hosts show the real OS cursor).

## 1b. Build and environment

- Build first, and **verify the build actually succeeded**:

  ```sh
  cargo build --examples 2>&1 | grep -cE "^error"   # must print 0
  ```

  A failed build leaves the *previous* binary in place; rerunning then silently
  tests stale code. This bit us more than once.
- The harness is a normal dependency of the host app — there is no feature gate
  in the harness itself. **The host decides** whether player-facing builds
  include it (they never should: it is a cheat/debug surface, BRP is
  unauthenticated, localhost bind only).
- The host's asset setup is its own concern (Bevy's usual `AssetPlugin` rules);
  nothing here needs special env vars.

## 2. Launch / teardown recipe

```sh
LOG=/tmp/bevy-playtest; mkdir -p "$LOG"   # the redirects below fail if it's missing
pkill -x <app-binary> 2>/dev/null; sleep 1
ss -tlnp | grep -E ":15702|:15710"          # harness ports free?

# app under test — put a hard lifetime on it (timeout N) and keep N big enough
# for the whole test; a dead app answers BRP with connection-refused (silent with -s)
(timeout 300 cargo run -q --example headless > "$LOG/app.log" 2>&1 &)
sleep 5

# sanity: the MCP server must have come up
grep -i "mcp tool server listening" "$LOG/app.log"
```

Hard rules learned the hard way:

- **Never `pkill -f <partial-path>`** — the pattern can match your *own shell's*
  command line and kills the test command itself (no output, no log file). Use
  `pkill -x <exact-binary-name>`.
- **Stale processes hold :15702/:15710.** Worse, a stale *old* process makes the
  port-based ready check pass while your freshly launched instance fails its own
  MCP bind (`AddrInUse` in its log) — so your curls hit the **old binary** and
  "your change did nothing". Always teardown first, verify with `ss`, and grep
  the fresh log for the MCP listening line.
- `timeout N` kills the app at N seconds wall-clock *including your thinking
  time between tool calls*. Budget generously; restart if it expires mid-test.
- Restart the app between test rounds unless you specifically want to test
  against existing state.

## 3. The tool API surface

JSON-RPC 2.0 over HTTP POST to the app's BRP port (default `http://127.0.0.1:15702`):

```sh
curl -s -m 6 http://127.0.0.1:15702 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"game/state","params":{}}'
```

Custom `game/*` methods provided by the harness (the host may register more — §11):

| Method | Params | Effect |
|---|---|---|
| `game/state` | — | The **host-registered** state snapshot (app state, player position/health, whatever the game exposes). Empty object if the host registered no hook — that is a host-app gap, not a harness bug |
| `game/client_info` | — | This host's effective launch configuration: `no_render`, ports, `screenshots_dir`, `screenshots_available`, offscreen `target_size`. Call first on a fresh session |
| `game/cameras` | — | Lists camera entities and their poses — the ids `game/screenshot {"camera": <id>}` accepts |
| `game/screenshot` | `{"label":"..."}`, `{"crop":[x,y,w,h]}`, `{"camera":<id>}` optional | Async capture; PNG written into the configured screenshots dir (persistent, never consumed — the human-browsable record). `crop` saves only that sub-rect — same pixel space as `game/ui` rects, clamped to frame bounds. Prefer cropped captures of a `game/ui` rect when inspecting one element: fewer vision tokens, and no provider downscale on the region of interest |
| `game/screenshot/get` | — | `{"ready":true,"png_base64":...,"path":...,"state":{...game/state...}}` for the newest capture. The `state` is sampled at poll time, so every capture arrives with its ground truth attached — never OCR the HUD. If the newest capture is pixel-identical to the last one served in full, responds `{"ready":true,"unchanged":true,"path","state"}` WITHOUT `png_base64` — don't re-request; read the state |
| `game/ui` | — | Accessibility-tree-style UI dump: every visible UI node's `rect` `[x,y,w,h]` **in the same pixel space `game/mouse move_to` consumes**, its text (button labels), `clickable: true` on interactive nodes (`bevy_ui::Interaction` holders), `interaction` (`Pressed`\|`Hovered`\|`Idle`), `pointer_hovered`, hovered-entity set, and the mocked pointer's position. Back-to-front render order. Read this to decide *what to click and where* — and crop screenshots to these rects — instead of estimating from pixels. **Headless caveat**: `interaction` stays `Idle` for all nodes on an offscreen target (bevy's `ui_focus_system` only updates `Interaction` for window-target cameras); `pointer_hovered`/`hovered_entities` are the reliable headless hover/press signal |
| `game/gamepad` | see §4a | Device-level gamepad mock (buttons/axes); drives UI navigation |
| `game/keyboard` | see §4b | Device-level keyboard mock (`ButtonInput<KeyCode>`) |
| `game/mouse` | see §4b | Device-level mouse mock: buttons, `move_to`, relative motion, wheel; drives real UI clicks through `bevy_picking` |

Bevy builtins are **`world.*`-named** in 0.19 (`world.query`, `world.get_components`,
`world.list_resources`, `world.get_resources`, `world.list_components`,
`world.mutate_components`, `world.spawn_entity`, `world.insert_components`,
`world.remove_components`, `world.reparent_entities`, `world.trigger_event`, …).
`world.query` schema:

```json
{"data":{"components":["<exact::TypePath>", ...],"option":["<exact::TypePath>", ...]}}
```

→ array of `{"components":{"<path>":{...}},"entity":<id>}`. The `components`
list = all must be present (and they are what gets fetched); `option` adds those
*only when present*; `filter.with` filters presence without fetching.

## 4. Input injection semantics

**Prefer gamepad-style input for UI navigation** — gamepad mocks are discrete,
level-triggered presses (hold/release — no pixel coordinates, no analog
calibration, no timing races like a mouse click's press/release pair), and they
flow through the app input crate's *real* binding resolution exactly like a
human's controller. Reach for `game/mouse`/`game/keyboard` when the task
explicitly targets those devices — §4b documents their extra gotchas.

Device-level mocks are the only kind in this crate: `game/gamepad` mocks a real
gamepad's button/axis state (`bevy_input::gamepad::Gamepad` on a synthetic,
lazily-spawned entity — created on first use, not at boot), `game/keyboard`
mocks `ButtonInput<KeyCode>`, `game/mouse` mocks `ButtonInput<MouseButton>` plus
real cursor motion/position through `bevy_picking`'s own event pipeline. The
point of device-level mocking: the value flows through the app input crate's
*real* binding resolution — dead zones, which context currently owns a shared
physical input, per-context device selection — exactly like a human's
controller. This is what makes UI navigation testable at all.

Gamepad state is **level-triggered, not duration-based** — a button/axis you set
stays exactly as you left it until you explicitly change it again, matching a
real controller. Budget an explicit release call for everything you press, or
use `{"input":"reset"}` once you're done with a scenario.

```sh
# press and release, like a human
{"input":"button","button":"South","pressed":true}
{"input":"button","button":"South","pressed":false}

# axes: roughly -1.0..1.0, e.g. left stick forward
{"input":"axis","axis":"LeftStickY","value":1.0}

# release/zero everything in one call — cheap insurance against a forgotten
# release leaving something stuck for the rest of the session; reach for this
# between unrelated test scenarios
{"input":"reset"}
```

Standard button names (19, matches `GamepadButton`, `Other(u8)` not exposed):
`South`, `East`, `North`, `West`, `C`, `Z`, `LeftTrigger`, `LeftTrigger2`,
`RightTrigger`, `RightTrigger2`, `Select`, `Start`, `Mode`, `LeftThumb`,
`RightThumb`, `DPadUp`, `DPadDown`, `DPadLeft`, `DPadRight`. Standard axis names
(6, matches `GamepadAxis`): `LeftStickX`, `LeftStickY`, `LeftZ`, `RightStickX`,
`RightStickY`, `RightZ`.

**The one trap that will cost you real debugging time if you don't know it
going in**: input crates' gamepad-button readers call `Gamepad::get`, which reads
the **`analog`** field — *not* `digital`/`ButtonInput`, despite that being the
obviously-correct-looking choice for a boolean button (confirmed by testing
against bevy_enhanced_input 0.26: an implementation using `digital_mut().press()`
compiled fine, ran with no error, and the UI simply never reacted — silent, not
a panic). `game/gamepad`'s own implementation already gets this right; this note
is here so nobody "fixes" it back to `digital` on a future refactor without
re-reading this.

**A UI element only reacts if the app gave it an `Interaction` component** (in
bevy, `Button` requires one via `#[require]`; hand-rolled widgets must insert
it). A node without `Interaction` is invisible to confirm/hover logic even when
it renders — check `game/ui`'s `clickable` flag before blaming input mocking.

## 4a. Seeing focus and driving navigation

Typical headless navigation loop: press a direction (e.g. `DPadUp`), release it,
re-read `game/ui` to see which row the focus moved to, then press the confirm
button (`South` is the near-universal confirm). Focused elements commonly draw a
focus ring (`bevy_ui::Outline`) — BRP
`world.query` with `"filter":{"with":["bevy_ui::ui_node::Outline"]}` finds the
focused element if the app draws one.

Whether a gamepad press moves focus depends on the app's bindings (bevy 0.19
has `bevy_ui::FocusPolicy`, directional-navigation helpers, and input-crate
`UiNavigate`-style actions; bevy's `bevy_ui::widget::Button` + `Interaction`
pair is the minimum). If nothing moves: check with BRP what actions/bindings the
app registered — this is an app-side gap, not a harness one. **[p19]**: the
prototype binds `Cardinal::dpad()` to `UiNavigate` octants and South/Enter to
`UiConfirm`, so DPad + South drive every menu — details in §12.

## 4b. Keyboard + mouse (`game/keyboard`, `game/mouse`) — real UI clicks too

```sh
# hold W through the actual KeyCode binding, then release
{"key":"KeyW","pressed":true}
{"key":"KeyW","pressed":false}

# release every held key
{"reset":true}
```

`key` is the exact Rust `KeyCode` variant name (`KeyW`, `Digit1`, `Escape`,
`Space`, `Enter`, `Tab`, `ArrowUp`, `ShiftLeft`, `ControlLeft`, …) — deserialized
directly via `KeyCode`'s own `serde` impl (the harness enables bevy's
`serialize` feature), so every one of Bevy's 160+ variants works, not a
hand-picked subset. Level-triggered like `game/gamepad`, not duration-based —
budget a release call, or use `{"reset":true}`.

**Edge-triggered consumers are unreachable** (confirmed in prototype_19,
playtest 0021 F2, and it is a bevy-mechanics fact, not an app one): the mock
writes land in `RemoteLast` (end of frame), and the next frame's
`keyboard_input_system` clears `just_pressed`/`just_released` before any
`Update` system runs — so `just_pressed`-reading consumers (a console toggle on
backtick is the classic case) can never fire from this mock. Only
level-triggered reads (`pressed`) work. Closest workaround: the app exposes a
custom BRP method/trigger for the behavior (§11), or use a real desktop window
(§4c).

`game/mouse` is discriminated by `input`:

```sh
# move the cursor to an absolute pixel position — the SAME space game/screenshot
# captures (offscreen target, or the primary window when there is one), so you
# can click exactly what you see in a screenshot. Read the target's rect off
# `game/ui` (its rects are in this exact space) rather than estimating from pixels.
{"input":"move_to","x":640,"y":400}

# press then release Left — this is a REAL click: it updates both
# ButtonInput<MouseButton> (for mouse-bound gameplay actions) AND fires a
# bevy_picking PointerInput on the pointer bevy_picking's own
# spawn_mouse_pointer already spawns at Startup (headless or not) — so it
# actually clicks whatever UI node is under the cursor, through the real
# hit-testing pipeline, not a shortcut
{"input":"button","button":"Left","pressed":true}
{"input":"button","button":"Left","pressed":false}

# relative motion (mouse-look) — dx/dy, like a real MouseMotion delta
{"input":"motion","dx":200,"dy":0}

# scroll wheel
{"input":"wheel","x":0,"y":1,"unit":"Line"}

# release all buttons + zero motion/scroll accumulators (does not recenter
# the cursor)
{"input":"reset"}
```

Button names: `Left`, `Right`, `Middle`, `Back`, `Forward` (matches
`MouseButton`). `Back`/`Forward` update `ButtonInput` only (no picking
equivalent). Wheel `unit` is `Line` (default) or `Pixel`.

**Verified live** (prototype_19, playtest 0022): clicking a menu button at its
`game/ui` rect transitioned the app's game state; hovering shows the hovered
state in `game/ui` rows; the whole click → hit-test → signal/Interaction
pipeline is the real one.

**The gotcha that cost real debugging time, same shape as the gamepad
analog/digital one**: `AccumulatedMouseMotion`/`AccumulatedMouseScroll` cannot
be set with a direct `world.insert_resource(...)` — Bevy's own
`accumulate_mouse_motion_system`/`accumulate_mouse_scroll_system` unconditionally
overwrite them from `MouseMotion`/`MouseWheel` **events** every single frame
(their own doc comments say "reset to zero every frame"), so a direct write is
silently wiped before any input crate's reader ever observes it — confirmed
live: look yaw stayed exactly `0.0` after a `dx:200` motion call with the
resource-write approach, no error anywhere. `game/mouse` instead writes real
`MouseMotion`/`MouseWheel` events (`world.write_message(...)`), letting those
systems compute the accumulated value on their own schedule, same as a real
winit event would. Don't "fix" it back to a direct resource write on a future
refactor.

## 4c. Real desktop-window testing (no offscreen target) — synthetic OS input quirks

Everything above mocks input at the ECS level: no real window, no real OS input
device. Sometimes you need the *other* thing — a real windowed app driven by
genuine synthetic OS input (a real uinput/Wayland device, indistinguishable from
actual hardware to the app) — e.g. to close the loop on something ECS-level
mocking can't test: an edge-triggered `just_pressed` consumer (§4b). The
harness's BRP/MCP surfaces work identically in a windowed host —
`game/state`, `game/screenshot` (falls back to the primary window when no
offscreen target exists), `game/keyboard`/`game/gamepad`, all BRP queries;
`game/mouse` targets the window's pixel space.

This section was learned on a real Sway (wlroots) session with two monitors;
take the specifics with a grain of salt on a different compositor, but the
*shape* of each gotcha (especially the acceleration one) is likely to recur
anywhere.

**Tool availability, this compositor**: `xdotool` **does not work at all** —
it's X11/XWayland-only, and a native Wayland surface isn't found by
`xdotool search --name ...`. `wtype` (the Wayland virtual-keyboard-protocol
tool) installed and ran with no error, but its key events **never reached the
app** — no visible effect, not even a console toggle — despite the window
holding real compositor keyboard focus (confirmed via `swaymsg -t get_tree`'s
`focused: true`). Wasn't root-caused (compositor config restricting the
virtual-keyboard protocol to specific clients is one guess). **`ydotool` is the
one that reliably works** — don't burn time on `wtype` first if it's not
already known-working on the target compositor.

`ydotool` needs setup, all one-time per session:
```sh
# ydotoold needs /dev/uinput access — check for an existing ACL first
getfacl /dev/uinput   # if it grants your user rw, no sudo needed at all
ydotoold --socket-path=/tmp/.ydotool_socket --socket-own=$(id -u):$(id -g) &
disown
export YDOTOOL_SOCKET=/tmp/.ydotool_socket   # needed by every ydotool call after this
```
`ydotool key <code>:1 <code>:0` (press+release) and `ydotool key <code>:1`/
`<code>:0` (hold/release separately) take raw Linux keycodes from
`/usr/include/linux/input-event-codes.h` (`KEY_W`=17, `KEY_ENTER`=28,
`KEY_ESC`=1, `KEY_UP`=103, `KEY_DOWN`=108, `KEY_GRAVE`=41, …) — not X11
keysyms, not the `KeyCode` names `game/keyboard` uses. `ydotool click <mask>`
buttons are **bit-flag hex**, not plain enum values — read the mask, don't
guess: `0x00` alone means "left button, do nothing" (down bit *and* up bit
both unset — a real, easy-to-make mistake, confirmed by testing: it compiles/
runs/exits 0 and produces literally no click at all); `0xC0` is a real
down-then-up left click; `0x40`/`0x80` are down-only/up-only (for a deliberate
held-drag). Mouse movement (`ydotool mousemove`) is **relative-only on this
compositor** — checking `cat /sys/class/input/eventNN/device/uevent` for the
`ydotoold virtual device` node showed `EV=7` (SYN|KEY|REL, no ABS bit at all),
meaning `--absolute` isn't backed by real absolute positioning hardware and is
at best a software approximation on top of relative deltas — don't trust it
for pixel-accurate targeting.

**The real gotcha, the one that cost the most time**: `ydotool mousemove`'s
relative deltas get warped by **libinput pointer acceleration** by default
("adaptive" profile) — a single large synthetic jump (e.g. "move by 900px to
reach a button") does not land 900px away, because acceleration curves are
tuned for continuous human motion, not one instantaneous synthetic delta.
Confirmed by testing: an absolute-feeling two-step move (pin to a corner with
a huge relative jump, then move by the exact target offset) landed wildly off
target with acceleration on, then landed pixel-exact once acceleration was
disabled. Fix, **per input device**, no restart needed:
```sh
swaymsg input "9011:26214:ydotoold_virtual_device" accel_profile flat
swaymsg input "9011:26214:ydotoold_virtual_device" pointer_accel 0
```
(get the exact device identifier from `swaymsg -t get_seats`, under
`ydotoold virtual device` — the vendor:product pair shown above is what that
session's `ydotoold` happened to register as, not guaranteed stable). With
acceleration flat, the pin-then-move-by-exact-delta pattern is reliable:
```sh
ydotool mousemove -x -5000 -y -5000   # slams into the top-left corner (0,0), any compositor clamps this
ydotool mousemove -x <target_x> -y <target_y>   # now a true relative delta from a known origin
```
**Re-pin before every click**, don't reuse a previously-computed origin — the
compositor's cursor-position bookkeeping does not appear to survive every
cursor-lock/unlock transition cleanly (e.g. an FPS game grabbing the cursor for
camera-look, then releasing it for a menu); a move that worked right after
pinning silently no-op'd once the cursor had been locked and unlocked again in
between, even with acceleration still flat. When in doubt, take a fresh
`grim -o <output>` screenshot and re-derive the cursor's actual last-known
position from the image rather than trusting your last computed target.

**Confirm target pixel coordinates from a *real* screenshot of the *actual*
resolution**, not a guess scaled from a downsampled preview — a rendered chat
image's stated "displayed at WxH, multiply by N" note is for *your* viewing
math only; once you load the actual PNG file (`PIL.Image.open(...)`), its
`.size` **is already the real resolution** — multiplying by the display
scale factor *again* on top of that is a real, easy mistake (confirmed by
testing: sampled the wrong pixels searching for a button, found nothing,
before realizing the file was already full-res). `grim -o <output-name>`
(`swaymsg -t get_outputs` for the name, e.g. `DP-2`) grabs the *whole
desktop*, not just the app window — use it whenever you need to see the
**real OS cursor** (a real screenshot shows the actual system cursor arrow;
`game/screenshot`'s in-app capture does not, since the cursor is
compositor-side, not part of the rendered frame) or confirm a window's actual
on-screen position/size (`swaymsg -t get_tree`, cross-checked against
`swaymsg -t get_outputs` for the output's own origin offset if there's more
than one monitor — a window's own `rect`/`geometry` is relative to its output,
not the global compositor space, unless that output happens to sit at `(0,0)`).

## 5. Probing the world (BRP)

Type paths must be **exact and fully qualified**. When in doubt, grep the source:

```sh
grep -rn "pub struct Mesh3d" ~/.cargo/registry/src/*/bevy_mesh-0.19*/src/
```

Known-correct paths (bevy 0.19): `bevy_camera::camera::Camera`,
`bevy_camera::camera::RenderTarget`, `bevy_camera::components::Camera3d`/`Camera2d`,
`bevy_camera::projection::Projection`, `bevy_mesh::components::Mesh3d`,
`bevy_pbr::mesh_material::MeshMaterial3d<bevy_pbr::pbr_material::StandardMaterial>`,
`bevy_transform::components::transform::Transform`,
`bevy_transform::components::global_transform::GlobalTransform`,
`bevy_ecs::hierarchy::Children` (not `relationship::`),
`bevy_ui::ui_node::ComputedNode`, `bevy_ui::focus::Interaction`,
`bevy_ui::ui_transform::UiGlobalTransform`.

**The single biggest trap: serialization failure ≠ absence.** Components holding
asset handles (`Mesh3d`, `RenderTarget`, …) cannot BRP-serialize
(`Arc<StrongHandle>` lacks `ReflectSerialize`) — they come back `null` in
`world.query` and as errors in `world.get_components`. To decide "is component X
present", call `world.get_components` with X listed explicitly and read the
`errors` map:

- `code:-23402 … did not register ReflectSerialize` → **present, unserializable**
- `code:-23403 … not present in Entity` → genuinely absent

`Assets<Image>` / `Assets<Mesh>` are not BRP-reflected at all — you cannot count
asset stores; count *entities* carrying handles instead.

Entity ids appear in **two formats**: BRP returns a large u64 bit-pattern, while
the app *log* prints `index+generation` form (`1224v1`). They are the same
entity — e.g. BRP `8589933367` ≡ log `1224v1` (roughly `2^33 − 1225`). Don't
conclude "different entity" when comparing a log line to a query result; convert
via the ±index relationship instead.

Rapid-fire BRP calls can transiently fail (empty body / `KeyError: 'result'`).
Retry once after ~2s before concluding anything.

## 6. Screenshots

**Screenshots are a last resort, not your eyes.** Understand the world through
the structured data surfaces first: `game/state`, `game/ui` (labeled rects +
text for everything on screen), and BRP's `world.query`/`world.get_components`.
They are cheaper, exact, and stable in a way pixels never are (a vision model
reading a rendered frame is the least reliable instrument in this toolbox).
Take a screenshot **only** when:
- the task explicitly asks for one, or
- the thing under test is *inherently* visual — rendering, lighting, materials,
  particles, camera framing, UI compositing/layout. (Even then, `game/ui` is
  the right tool for UI *content*; pixels only answer "did it render right".)

For gameplay, UI, and logic testing, plain MCP/BRP data should be sufficient —
and if it isn't, **that's an API gap to report and fix** (a field missing from
`game/state`, a query the dump doesn't expose — hosts add these via custom BRP
methods, §11), not a reason to fall back to reading pixels. File the gap in
your report (§9) instead of squelching it with screenshots.

- `game/screenshot {"label":"ingame"}` →
  `<screenshots_dir>/<millistamp>-ingame.png` (persistent, never consumed —
  the human-browsable record). Poll `game/screenshot/get` until `ready:true`
  (~1–3s), or just `ls` the configured screenshots dir.
- In headless mode the capture reads the **shared offscreen texture** = exactly
  what the agent "sees". `{"crop":[x,y,w,h]}` (read the rect off `game/ui`)
  costs fewer vision tokens and keeps full effective resolution on the region of
  interest. `{"camera": <id>}` renders one camera's view (its render target is
  borrowed for one frame, then restored).
- The response embeds the `game/state` payload sampled at poll time, and the
  same payload is written to a `<capture>.json` sidecar next to the PNG.
- Annotate captures by measuring pixels, not by eyeballing memory: unique-color
  counts via PIL tell you instantly whether a frame rendered (hundreds of
  colors), is the clear color (1 color), or is a menu (~200 colors).

## 7. Known failure modes & recovery

| Symptom | Cause | Recovery |
|---|---|---|
| Empty curl bodies | App dead (timeout expired) or malformed shell quoting | `ps aux \| grep -c "[<binary-name>]"`; rebuild the curl |
| Curls return nothing *and* app alive | **zsh doesn't word-split unquoted vars** — `H='-H …'; curl $H …` passes one giant arg | Always write literal URLs/headers in curls |
| Fresh instance logs `mcp server stopped: AddrInUse` — **but BRP still answers** | A stale process from a previous run holds both ports; your port-based ready check passed against the *old* process and your curls are hitting *its* surfaces (your change will look like it "did nothing") | Kill the stale process (`pkill -x`), verify `ss` shows nothing, relaunch, confirm the fresh log's `mcp tool server listening` line |
| `game/screenshot` errors "no rendering enabled" | Host configured `no_render` — correct behavior | Use `game/ui` + BRP queries, or relaunch rendered |
| `game/state` returns `{}` | The host registered no `state_snapshot` hook | Ask for the hook, or fall back to BRP `world.query` on the game's own components |
| Test results look impossible / old behavior | Stale binary from a failed build | Rebuild, `grep -cE "^error"` must be 0 |
| `pkill -f` kills your own test command | `-f` matches your shell's own command line | Use `pkill -x` |
| Rendered host hangs at startup: log stops after `SystemInfo`, no `AdapterInfo`, no window | NVIDIA's Vulkan ICD calls `XOpenDisplay` during instance creation whenever `DISPLAY` is set; a wedged or never-started Xwayland leaves that `connect` blocked forever (main thread in `unix_wait_for_peer`; `ss -xl` shows `/tmp/.X11-unix/X0` with a full backlog, `LISTEN 1 1`) | Launch with `env -u DISPLAY` (winit uses Wayland anyway); restarting the compositor restores Xwayland. Not an app bug |

## 8. Multi-instance / fleet testing

Every extra headless instance past the first needs its own ports (the defaults
are per-host singletons — a second instance on the defaults fails with
`AddrInUse`):

```sh
# instance N: BRP on 1600N, MCP on 1700N (via McpHarnessConfig::from_env flags)
<app-binary> --brp-port 1600$N --mcp-port 1700$N
# address instance N's BRP at http://127.0.0.1:1600$N (JSON-RPC POST, same methods)
```

A non-default BRP port also isolates captures into a per-instance
`screenshots/client-<port>/` dir (a shared dir would make one instance's
`game/screenshot/get` return another's capture). Routing needs no proxy — BRP
is stateless JSON-RPC POST, so "talk to instance N" is just addressing its
port.

A common fleet shape: one rendered host (the fleet's *eye* — screenshots on
demand, including `{"camera": id}` captures) serving vision for many render-less
instances that run gameplay/logic. Budget cores per instance accordingly and
measure on your target machine.

For client/server games: a *server* binary can host the harness too (its own
ports, its own custom methods) so an agent can compare authoritative server
state against a client's predicted view when debugging desync. **[p19]** does
exactly this — see §12.

## 9. Reporting

**Every run gets a report, no exceptions.** Any session where an app gets
started, driven through the MCP/BRP tool API in any way, and then torn down — a
full formal state tour, a five-minute poke to sanity-check one thing, a
targeted bug-reproduction pass, a one-off check while debugging something else —
gets a filed report. "This was too small/informal to write up" is exactly the
case this rule exists to rule out: the value is in the accumulating, searchable
history (what was tried, what was observed, on what date, against what commit),
not in any single run being significant. Don't wait to be asked.

**Layout convention** (adopted from prototype_19; adapt the directory names to
the host repo):

- One Markdown file per report: `<reports-dir>/playtest_NNNN.md` (find the next
  free number; four digits, zero-padded; never reuse or renumber). GitHub-
  flavored Markdown, tracked in git, renders directly — no build step.
- Curated screenshots this report actually references live in a sibling
  `screenshots/playtest_NNNN/` directory — copy in only what's worth keeping
  from the raw capture staging dir, don't dump every capture. If the repo uses
  Git LFS for binaries, keep screenshots LFS-tracked and the reports plain
  git (prototype_19's `.gitattributes` pattern covers
  `docs/agents/playtests/screenshots/**/*.png` — see §12).
- An `index.md` (newest first: date, commit, agent, link, one-paragraph
  summary) is part of filing a report, not an optional later chore.

**House style**:

- `# Agent Playtest NNNN — <title>` as the only H1, then a two-column metadata
  table (`| Field | Value |`) with at least Date, Commit, Agent, App/mode.
- `##` for sections (Purpose, Method, State tour/Verification, Findings, …),
  `###` below that. Name the findings section `## Findings` so other documents
  can link `playtest_NNNN.md#findings`.
- Figures: a relative image link plus an italic caption paragraph directly under
  it — `![<file>.png](screenshots/playtest_NNNN/<file>.png)` then `*<caption>*`.
- Findings as bold-led paragraphs or bullets (`**F1 — <headline>**: …`); results
  tables as ordinary Markdown tables; command output in fenced code blocks.
- Escape a literal `|` inside table cells as `\|`, and a literal `<word>` outside
  code as `\<word>` (GitHub would otherwise swallow it as an HTML tag).

**The findings section isn't just confirmed bugs.** Record observations,
suspicions, things that looked odd but weren't chased down, open questions,
anything that would help a *future* session pick up the thread faster — not
only what got definitively proven. **Tool-API gaps belong here too**: if
`game/state`/`game/ui`/BRP didn't expose something you needed to understand the
world and you were tempted to read it off a screenshot instead (§6), write down
exactly what was missing — those gaps get fixed (a custom BRP method on the host,
or an upgrade to the harness), and every one reported makes the data-first
workflow cover more. Say what's uncertain as uncertain; don't inflate a hunch
into a confirmed finding, but don't omit it either. Cross-reference earlier
reports by number when a run confirms, contradicts, or narrows something an
earlier one said.

**The report's metadata table needs a `Commit` field** — the git `HEAD` the run
was actually performed against, so the chronology and the actual code under test
stay unambiguous later. Don't guess from memory or approximate dates:
cross-reference the run's own screenshot timestamps (millisecond epoch in the
filename — `date -d @<ms/1000>` or equivalent) against
`git log --format="%h %ci %s"`, then confirm the candidate commit's changed
files actually match what the session touched (`git show --stat <hash>`) before
trusting a timestamp match alone. If code changed *during* the run (a
diagnose-and-fix playtest, not a pure state tour), a single hash can be actively
misleading — record both the starting commit and the one any fix landed as, and
say so explicitly, rather than picking one and implying it covers the whole run.

If the host repo has a human-only documentation area, **never write into it** —
reports go in the agent-facing area only. **[p19]** enforces this as
`docs/humans/` (see §12).

## 10. Practical flow summary

1. Teardown (`pkill -x`), verify harness ports free (§2).
2. Build; confirm zero errors.
3. Start the app (hard `timeout`, log to a file), confirm the MCP listening
   line in the log (§2).
4. `game/client_info` first (§1a).
5. `game/ui` + `game/state` (screenshot only if the task is visual — §6).
6. Drive with `game/gamepad`/`game/keyboard`/`game/mouse`; sample `game/state`
   and BRP queries after each action. Screenshots only on explicit request or
   for inherently visual checks; report any data-surface gap you hit (§9).
7. Capture logs; teardown when done (or leave the process for the user, saying
   which processes are yours).
8. Write the playtest report and its index entry (§9).

## 11. Host-app integration checklist

For a game/app that wants to be drivable by this playbook:

- Add `bevy_mcp_harness` and `BevyMcpHarnessPlugin` (dev/QA builds only).
- Windowed app: `BevyMcpHarnessPlugin::default()`. Headless agent host: compose
  without winit yourself (see `examples/headless.rs`) and set
  `offscreen_size: Some(...)`; add `no_render: true` for GPU-less hosts.
- Register a `state_snapshot` hook so `game/state` carries your game's real
  state (position, health, app states) — this is what makes the data-first
  workflow (§6) possible at all.
- Add custom BRP methods for game-specific actions/queries (menu flows, spawns,
  combat) — `game/state` + `game/ui` + `world.*` cover inspection; actions need
  your own methods. See `AGENTS.md` "Extension points".
- Wrap the action-shaped ones as `extra_tools` MCP tools so a pure-MCP agent can
  use them without knowing your BRP port layout.
- Mark interactive widgets with `bevy_ui::Interaction` (or derive `Button`) so
  `game/ui` reports them `clickable` and confirm/hover logic can reach them.

## 12. prototype_19 specifics

The harness was extracted from prototype_19 (`p19`, a Bevy 0.19 multiplayer
game: client/server over lightyear, Avian physics, bevy_enhanced_input,
bevy_markup UI, Steam Deck as min-spec target). The learnings below only apply
when testing *that* app (or one sharing its architecture); they are kept here so
the generic sections above stay clean and nothing learned is lost.

### Launch flags and modes (p19's own CLI, read pre-sync by `p19-client`)

| Configuration | Launch flags | Renders? | Screenshots? | World visuals? | Typical use |
|---|---|---|---|---|---|
| Headless agent host (default) | `--mcp` | yes, offscreen 1280×800 (Steam Deck 800p) | ✓ (crop, unchanged-suppression) | ✓ load + replicate | Full playtesting, visual checks included |
| GPU-less agent host | `--mcp --no-render` | **no** (no Vulkan needed at all) | ✗ clean error | ✗ never load (implies `--no-common-assets`) | Gameplay/UI/logic fleets on small boxes; ~0.7 core + ~0.3 GB vs ~1.3 cores + ~1.1 GB |
| `--no-common-assets` | alone or implied | yes | ✓ | none via manifest — content only via `ClientWorldAsset`s by path; fonts/sounds/icons fall back to embedded/`None` | Plaintext-asset-root playtesting |
| Windowed dev client | none, but built with `--features dev-tools` (not a default feature) | real window | ✓ | ✓ | Human-visible sessions |
| Fleet member | `--mcp --brp-port N --mcp-port N` | per above | ✓ (isolated dir) | per above | Many clients, one server |
| **Observer** | `--mcp --headless-render` | yes, at **2 fps** (logic still 60 Hz via catch-up) | ✓ on demand | ✓ load + replicate | The fleet's *eye*: joins with **no player character** |

Observer particularities: join with `game/trigger {"event":"observe"}` (sends
`ObserveRequest` — joins the game room without spawning a player; `game/state`
never shows a `position`, that's correct); aim the `ObserverCamera` via
`world.mutate_components` on its `Transform`, then
`game/screenshot {"camera": <id>}`. The 2 fps loop means logic runs in batched
catch-up ticks — don't use the observer for timing-sensitive measurement.
Only `bevy_mod_outline`/`bevy_hanabi`/FPS-overlay plugins are skipped under
`--no-render` — "particle effect fired" / "outline appeared" cannot be tested
there. `--no-render` implies `--mcp` + `--no-common-assets`; `game/client_info`
reports the effective flags.

### The three processes (p19)

| Process | Binary | Ports |
|---|---|---|
| Game server | `target/release/p19-server` | UDP :6000 (game) · HTTPS :6001 (connect-token endpoint) · server QA: BRP :15701 + MCP :15711 (`server/state`) |
| Client (headless agent host) | `target/debug/p19-client --mcp` | BRP :15702 · MCP :15710 |
| You (the agent) | shell + curl/python | talks to :15702 |

The server exposes `server/state`: authoritative app state, every connected
client, every live player's server-side transform/HP/owning connection.
Compare it against a client's `game/state` when debugging desync (the client's
view is predicted and room-filtered).

### p19-only harness methods (lived in `dev/tool_api.rs`; not ported to this crate)

| Method | Params | Effect |
|---|---|---|
| `game/input` | §5 of the p19 playbook | Action-level `ActionMock` on the replicated BEI action entities (`Movement`/`Jump`), plus `rotate` (radians total, spread across `ticks`, mocking the client-local camera action — ADR 0017). Rides the exact replicated-BEI/prediction pipeline. Bypasses binding modifiers. **Blocked while the dev console or pause modal is open** (`gate_replicated_input_context` deactivates the whole `PlayerInputContext`) |
| `game/trigger` | `{"event":"connect"\|"play"\|"observe"\|"disconnect"\|"spawn_cube"\|"spawn_npc"\|"attack"\|"kill"}` | Fires the app's own client-local events — the same ones the menu buttons / hotkeys fire |
| `game/select` | `{"entity":..}` / `{"nearest":true}` / `{"name":".."}` | Injects crosshair targeting headlessly (`Selected`); `nearest` excludes the local player by entity (a `Without<LocalPlayer>` filter on a resource excludes nothing). Pair with `game/trigger attack\|kill` — the crosshair raycast needs a real window |
| `game/levels` | — | Server-replicated `Levels` singleton (lobby only) |
| `game/select_level` | `{"asset_path":"levels/minimal.level.ron"}` | Sends `LoadLevelRequest` (the lobby picker's exact message) |

### Get-in-game sequence (p19)

```sh
# 1. connect → poll game/state until game_state == "Lobby" (~2s)
curl … "game/trigger" '{"event":"connect"}'
# 2. game/levels → pick asset_path (only level: levels/minimal.level.ron)
# 3. select_level — ONLY on a fresh server; skip if already in-game
curl … "game/select_level" '{"asset_path":"levels/minimal.level.ron"}'; sleep 3
# 4. play → poll until "position" appears (player spawns at (0, 0.92, 0), grounded, 100 HP)
curl … "game/trigger" '{"event":"play"}'
```

State-machine facts: `InGameRequest` is handled only while the server is in
`ServerState::InGame` and **silently dropped otherwise** (e.g. while `Loading`).
`select_level` while a level is `Loading`/`LevelLoaded` is **rejected** by the
server's `LevelState` guard — restart the server to switch levels. `position`
appears only once the client has `Controlled` on its player;
`game_state:"InGame"` *without* `position` means you inherited a zombie player's
`ClientInGame` on reconnect — restart the server. `connected:false` in
`game/state` can lag reality; trust the client log's `connected to server` line.
Connect requires the token endpoint (:6001); the server's self-signed cert is
pinned trust-on-first-use (`assets/client/network/token-tls-fingerprint.txt`;
delete to re-trust a rotated cert).

### p19 input specifics

- **Default input method: gamepad** — the Steam Deck is the primary/min-spec
  target, so a playtesting agent emulates a Deck player: drive UI with
  `game/gamepad` (DPad navigation + South to confirm) and gameplay with
  `game/input`/`game/gamepad`.
- `game/input` examples (ticks ≈ 16.7 ms each; `{"action":"movement","x":0,"y":1,"ticks":120}`
  ≈ 24.7 units at full deflection; jump is a 2-tick press, airtime shorter than a
  capture round-trip; rotate's `yaw_delta` is radians total, positive = turn
  right / `pitch_delta` positive = look down — measured: yaw 0.5/pitch 0.2 →
  camera and server both (−0.5, −0.2); yaw 0 faces −Z per ahoy's `CharacterLook`).
- UI navigation specifics: the pause modal opens on `Start` (bound in
  `PlayerControls`); "Resume" auto-focuses; `DPadUp` moves focus to "Main Menu";
  South **and Enter** are `UiConfirm`'s bindings (both emit the focused
  element's `data-on-click` signal, same as a mouse click). The focused element
  draws an `Outline` while `InputFocusVisible` is set — find it via
  `world.query` for `bevy_markup::html::HtmlElement` with
  `"filter":{"with":["bevy_ui::ui_node::Outline"]}`. Every clickable element has
  a stable `id`; focus survives rebuilds by id. Wheel/edge paging in a selector
  re-renders its 5 rows (`slot-0`..`slot-4`) — re-read `game/ui` after each step.
- Known cosmetic limitation: `game/gamepad` doesn't flip `InputDeviceState` to
  `Gamepad` (control-tip icons stay keyboard-styled) — that state tracks real
  input *events*, the mock writes persistent component state. Every actual
  gameplay/UI effect is real.
- Enter works headlessly only since the bevy_markup migration (ADR 0015) —
  before it, `bevy_input_focus::dispatch_focused_input` needed a `PrimaryWindow`.
  Every button is now confirmed by `MenuControls`' `UiConfirm` action (gamepad
  South *and* Enter), which needs no window (playtest 0022).
- Real-window windowed-client note: `game/mouse`'s `move_to`/`button` in the
  p19 version required the `OffscreenRenderTarget` (only in `--mcp` mode) —
  in a real window `move_to` errored cleanly and `button` set the raw
  `ButtonInput` but never generated a real click. The harness generalization
  (pointer target = offscreen **or** primary window) supersedes this; the p19
  code predates it.

### p19 failure modes & recovery (not generic)

| Symptom | Cause | Recovery |
|---|---|---|
| `game/state` says InGame but no `position`, forever | Zombie-player inheritance: a previous client's `Lifetime::Persistent` player replicated its `ClientInGame` to your fresh connection without `Controlled` | Restart the server |
| `select_level` appears to do nothing | A level is already `Loading`/`LevelLoaded`; the server's `LevelState` guard rejects further requests (logged server-side) | Just `play`; restart the server to switch levels |
| Server floods `server_late_input_mismatch` when a second client joins | Join-burst replication hitch blows the 2-tick input-delay headroom; self-heals in ~10 ticks | Benign — don't fix |

### Multi-client with a human (p19)

The user's windowed client and your headless client can share a server: user
gets in-game normally; you launch `--mcp`, `connect`, **skip `select_level`**
(the server is already in-game; a second request is rejected anyway) and `play`
directly. All players share one spawn point — if the user stands there you
spawn stacked on their head (playtest 0018); have them step away first.
Expect the benign `server_late_input_mismatch` burst during your join.
Capacity: each rendered `--mcp` client is a software-Vulkan (lavapipe) Bevy
instance; measured ~10 clients ≈ 13 cores + ~1 GB each — budget accordingly.
Each instance generates a fresh nanosecond netcode client-id — no collision.

### p19 visual divergences in `--mcp` (mechanics in `AGENTS.md`'s headless camera section; investigations: playtests 0003/0004)

- Menu/lobby: UI and the real `.glb` background both render, UI composited on
  top. The in-game HUD (including the crosshair) renders too.
- In-game: the starfield HDRI skybox and level geometry render correctly.
  `levels/minimal.level.ron`'s floor renders black specifically because that
  level's content has zero light entities anywhere — not a `--mcp` bug,
  confirmed via BRP, would be black on a windowed client too.

### p19 reporting/documentation system (the conventions §9 generalizes)

- `docs/humans/` is human-only: never create, edit, move or delete anything
  there (see p19's `AGENTS.md` "Rules"). Reports go in `docs/agents/` only.
- Playtest reports: `docs/agents/playtests/playtest_NNNN.md` + index
  (`index.md`, newest first) + curated screenshots under
  `docs/agents/playtests/screenshots/playtest_NNNN/` (Git LFS via
  `.gitattributes`; confirm `git lfs status` shows them as LFS objects before
  committing). Raw capture staging: `docs/agents/playtests/dist/screenshots/`
  (gitignored, never committed).
- Bug reports: `docs/agents/bug_reports/bug_XXXX.md` + ledger in its
  `README.md` (see `skills/bugreport.md`).
- Skills: `docs/agents/skills/{playtest,bugreport}.md`; decisions:
  `docs/agents/adr/` (0009 tool API, 0011 vision/fleet, 0012 no-render,
  0015 bevy_markup, 0017 client-owned look).
- Isolated playtest assets: `docs/agents/playtests/playtest_assets/
  playtest_NNNN/{server,client}/assets/` — fully plaintext (hand-written JSON
  `.glb` with Skein components, a per-run `common_assets.assets.ron` remap,
  `.level.ron`, `config.toml`, one locale), launched via `BEVY_ASSET_ROOT`.
  Fully optional engine furniture via `#[asset(key = "...", optional)]`
  Option handles (playtest 0010), or skip the manifest entirely with
  `--no-common-assets` (placeholder collection, immediate MainMenu). The
  recommended mode is still the trimmed-manifest one (keeps `MeshPrimitive`
  world visuals). Template: `playtest_assets/playtest_0009/` + its report.
- Build check for the harness gate: `cargo build -p p19-client --features
  dev-tools` (the tool API is behind the `dev-tools` cargo feature — works in
  dev *and* release; the gate is the feature, never the profile).
- Asset roots resolve via `p19_shared::paths::asset_dir`; `BEVY_ASSET_ROOT=<dir>`
  points a binary at an isolated asset set (`<dir>/assets/` is used). Don't set
  `CARGO_MANIFEST_DIR` — it no longer affects asset loading.
