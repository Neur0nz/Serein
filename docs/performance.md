# Performance findings

## Startup restore and optimistic writes

Compared `058e3357` with the startup and optimistic-write changes using locked Rust 1.99.1
release builds of `replay-bench`. Both binaries ran on the same Linux x86-64 host with an Intel
Xeon Platinum 8573C. The process was pinned to one logical CPU. Each binary ran one warmup and
five measured samples, alternating the order between samples. Repeat with `cargo replay` and
`cargo replay --ack-reconciliation`, then run the built binaries directly for sampling.

| Reducer workload | Baseline median (range) | After median (range) | Median delta |
| --- | ---: | ---: | ---: |
| 100,000 message events | 82.613 ms (80.925–100.985) | 86.107 ms (81.393–92.443) | +3.494 ms |
| Eight bulk acknowledgments of 1,024 channels | 53.365 ms (48.811–56.426) | 50.064 ms (49.173–119.582) | −3.301 ms |

The runs retained the same 500 timeline records and 417,492–417,977 estimated bytes. The timing
ranges overlap, with one slow bulk acknowledgment run. These samples cannot establish a timing
change. This workload measures reducer reconciliation. Native startup timing, process CPU and
memory under interaction, REST scheduling, and comparisons of standard package sizes remain
unmeasured.

## Desktop startup baseline — November 21, 2026

Baseline `39780338` was the head of the fork's instant-shell and optimistic-writes PR. That
revision added a three-column skeleton during saved-login restore, a SQLite-backed preview of the
last navigation and chat, and a handoff to live Gateway state. It also keeps the extension
start-screen override. The navigation snapshot cap is 512 KiB serialized, 200 guilds and
2,000 channels. Optimistic writes and close draining make up most of the PR. Its
eight CI jobs completed successfully. [PR #1](https://github.com/Neur0nz/Serein/pull/1) ·
[CI](https://github.com/Neur0nz/Serein/actions/runs/44876147177)

The rewritten PR at `5340c069` keeps the skeleton through a deferred connection and clears
the pending connection on cancel. It shows placeholders until READY verifies the account,
then loads one remembered channel ID and opens it through normal history if permissions
allow and the user has stayed on the default view. That removes the original draft's
roster inference, pre-READY message hydration and duplicate history scheduling. Check for
a possible intermediate Home frame while the post-READY selection read is pending.
[Startup rewrite](https://github.com/Neur0nz/Serein/commit/48e82640c6130308376b29624d0417ff04173d54)

### First visible content on offline Linux at `39780338`

Built with pinned Rust 1.99.1 and `cargo build --locked --offline -p serein --features demo
--bin serein` using the dev profile with `CARGO_PROFILE_DEV_DEBUG=0`. Ran on Linux x86-64,
kernel 6.18.57, Xeon Platinum 8573C, 4 CPU quota and 16 GiB memory limit. WGPU selected
Vulkan on llvmpipe in a 1440 × 1000 Xvfb display. Every launch had a private profile or
reused the initialized no-account profile for the repeat case. The display had a private
D-Bus session, and the container had networking disabled. Each scenario used one warmup
and 15 direct process launches. The probe polled X11 about every 4 ms and recorded when the
window was mapped and when the expected region first contained varied app pixels.

| Scenario | Median mapped | Median visible content | Visible content range |
| --- | ---: | ---: | ---: |
| New no-account profile | 1,196 ms | 1,266 ms | 1,166–1,577 ms |
| Repeat no-account profile | 1,000 ms | 1,080 ms | 882–1,383 ms |
| Ordinary `--demo` | 1,300 ms | 1,485 ms | 1,306–1,697 ms |
| Skeleton restore fixture | 1,072 ms | 1,132 ms | 1,006–1,432 ms |
| Cached restore fixture | 1,139 ms | 1,242 ms | 1,040–1,950 ms |

The restore fixtures use `--demo --demo-login` with `--demo-restoring` or
`--demo-restoring-cached`. The latter seeds navigation and messages directly. The cases
ran in groups on one software-rendered debug build. The ranges overlap and the workloads
differ. Packaged release timing, physical-display presentation, input response, real
saved-account hydration, Gateway READY and time to fresh messages remain unmeasured.

### Launch phase trace at `5340c069`

The instrumented normal debug build used pinned Rust 1.99.1 in an offline Linux Xvfb
session with llvmpipe Vulkan and a private profile and D-Bus. There were eight fresh
no-account launches and eight repeat no-account launches, alternating order after a
warmup. Fresh used a new profile per trial. Repeat reused an initialized database, a
default-equivalent preferences row, an empty device file and the Mesa cache. The
observer and the app use Linux `CLOCK_MONOTONIC` and the observer polls X11 every 4 ms.

| Phase | Fresh median | Repeat median |
| --- | ---: | ---: |
| Spawn to entry into Rust main | 83.82 ms | 77.44 ms |
| Rust main to native setup | 25.86 ms | 4.55 ms |
| Window and WGPU setup | 192.54 ms | 137.82 ms |
| Desktop constructor | 356.54 ms | 365.00 ms |
| First frame through return from `Queue::present` | 515.96 ms | 282.70 ms |
| Return from present through observed X11 pixels | 111.34 ms | 62.14 ms |
| Spawn to observed sign-in pixels | 1,368.26 ms | 957.20 ms |

The individual medians come from different trials. In the representative repeat launch,
83.47 ms went to reaching Rust main, 4.38 ms to pre-window setup, 140.58 ms to native
setup, 360.55 ms to desktop construction, 0.33 ms to entering the first frame, 265.01 ms
to producing and presenting the first frame, and 109.37 ms to observing the pixels in X11.
The total was 963.68 ms. All trials presented on their first root frame.

- **Defer asset work.** Repeat medians inside the constructor were 227.31 ms for Twemoji,
  52.97 ms for the icon atlas and 12.23 ms for video pipelines. The first frame staged
  19.86–20.23 MB of CPU-side texture pixels, including 16.52 MB from Twemoji. Gate both
  preparation and staging on use, and keep first-screen assets available. Upstream #1040
  already explores demand-driven emoji. [#1040](https://github.com/ViceVerse-cz/Serein/pull/1040)
- **Profile the first UI pass.** Desktop UI callbacks took 185.97 ms median on repeat.
  Texture staging took 45.95 ms and the present call took 49.70 ms. These are separate
  spans. Break UI time down by the view drawn, text layout and any synchronous setup in
  a release build. Test first visible pixels and the response to a local click separately.
- **Probe the renderer and storage paths.** Five Vulkan-only repeats chose the same
  llvmpipe adapter. WGPU instance initialization was 41.44 ms median versus 66.99 ms with
  Automatic. The end-to-end ranges overlap. Investigate unnecessary backend probing
  without weakening adapter fallback. SQLite open and initialization took 24.07 ms fresh
  and 2.11 ms on repeat. Measure migration and lock waits before redesigning persistence.

The minimum path with this renderer includes a window, a surface-compatible device, the
core UI pipeline, basic text and chrome, and a frame the window system can display. Establish
a practical floor by measuring a minimal release eframe app and release Serein with the same
backend, adapter and window configuration on target hardware. The trace covers debug software
rendering and observed X11 pixels. Input response, physical-display timing and authenticated
startup remain unmeasured. Credential handling, READY identity and permission checks, and live
history have separate prerequisites from the first window.

### Release first UI pass with emoji deferred

A local production-feature trace based on `6e5c6c85` leaves Twemoji unloaded during
sign-in. This prototype isolates the remaining first-frame work. Upstream #1040 supplies
the proposed emoji implementation. The trace uses locked Rust 1.99.1, fat LTO, offline
Xvfb and llvmpipe Vulkan at 1×. It reports five fresh profiles and five repeat profiles
after warmup. Every launch exited cleanly and drew sign-in in one egui pass.

| Launch phase | Fresh median, ms | Repeat median, ms |
| --- | ---: | ---: |
| Spawn to Rust main | 65.06 | 69.15 |
| Rust main to native setup | 15.10 | 3.64 |
| Native window and WGPU setup | 175.38 | 112.95 |
| Desktop constructor | 78.82 | 71.32 |
| First frame through return from `Queue::present` | 264.36 | 62.58 |
| Present return to observed X11 pixels | 21.16 | 18.88 |
| Spawn to observed sign-in | 597.58 | 346.94 |

Phase medians come from different launches. Within native setup, WGPU instance
creation took 95.75 ms fresh and 58.74 ms repeat. The X11 window creation call took
1.34 ms and 1.22 ms. The first present call alone took 188.36 ms fresh and 31.98 ms
repeat on this software driver. SQLite open took 13.52 ms fresh and 1.34 ms repeat.
Measure WGPU backend selection, first presentation and legacy or locked stores as
separate workloads. Their costs and required behavior differ.

| Phase | Fresh median, ms | Repeat median, ms |
| --- | ---: | ---: |
| Eager 64 and 32 px icon layers in constructor | 17.19 | 14.50 |
| Video converter setup in constructor | 7.42 | 4.05 |
| First app logic | 0.57 | 0.40 |
| First app UI | 18.24 | 20.23 |
| Visible 16 and 24 px icon layers in first app UI | 12.73 | 14.16 |
| Text layout in first app UI | 2.78 | 3.19 |
| Translation lookups in first app UI | 1.40 | 1.83 |
| Native login HTTP client creation in first app UI | 0.17 | 0.16 |
| First-frame WGPU texture staging | 34.14 | 5.02 |

The spans include their children. All ten launches queued 3,715,072 bytes of CPU-side
texture data on the first frame. The unused 64 and 32 px icon layers account for
2,293,760 of those bytes. The visible layers still need preparation on a cold draw.
The background system-font worker ran, and the default English first pass made zero
system-font fallback calls. Keep the auth startup ordering and prioritize the measured
asset work visible at 1×. Compare the separate icon branch against its session-restore
parent, and repeat the first frame on #1040 after integration.
[Prototype measurements](https://github.com/Neur0nz/Serein/pull/2) ·
[Upstream emoji rasters](https://github.com/ViceVerse-cz/Serein/pull/1040)

In one exploratory trace of a click on the first visible Settings gear, the event took
142 ms to reach the handler and another 23 ms to reach the first present return. The
Settings view itself took 5.4 ms in that frame. The first observed Settings pixels arrived
316 ms from input. The observer cannot attribute those pixels to a particular present.
The fixture dismissed Settings and needed forced exit after timing. Compare focus in a
sign-in field separately from Settings so input scheduling and the cost of loading the
full pane can be judged independently.

### First sign-in layout

The first-focus probe on the packaged prototype missed the password field during an
approximately 35 px layout shift. One click after the field settled reached its focus
ring 84 ms later. That exploratory run needed forced cleanup after timing, so the planned
repeat cohort did not run.

Native debug builds at `5340c069` and `9a1e9b34` ran offline with
`--demo --demo-login=new --demo-light`. At 1× and 1120 × 760, the first parent frame put
the Log In button at y=473 px, and the settled frame drew it at y=416 px. The candidate
drew it at y=416 px in both captures. The candidate's first and settled images matched
the parent's settled image pixel for pixel. The 1.25× and 2× captures also matched their
settled images. The observer did not catch a separate parent transition at those scales.
All six demo launches exited cleanly. The capture checks geometry and appearance. The
packaged tests measure startup latency separately.

### Packaged comparison for icons and sign-in layout

Compared the session-restore parent `5340c069` and the independent startup candidate
`9a1e9b34`. Built both standard voice-enabled Debian packages with Rust 1.99.1 and the
locked fat-LTO release profile. Both packages passed the 247-file smoke check. The
experiment isolates icon-layer loading and sign-in layout from the proposed upstream
emoji change in #1040.

The Xeon Platinum 8573C runner had a four-CPU quota and 16 GiB of memory. Both builds
used a private 1440 × 1000 × 24 Xvfb session at 1×, llvmpipe Vulkan (LLVM 19.1.7),
private D-Bus, and disabled networking. Fresh used new no-account profiles and Mesa
caches per launch. Repeat reused a warmed profile and Mesa cache per build. The OS
page cache was left intact. One warmup per build and profile preceded 15 alternating
pairs. The observer polled the native window approximately every 4 ms. All 60 measured
launches exited cleanly.

| Profile and milestone | Parent median (range), ms | Candidate median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Fresh, mapped window | 935 (644–1,479) | 845 (639–1,216) | −128 |
| Fresh, first sign-in pixels | 945 (658–1,497) | 854 (694–1,229) | −121 |
| Repeat, mapped window | 519 (391–926) | 696 (396–1,580) | +119 |
| Repeat, first sign-in pixels | 528 (397–934) | 697 (403–1,591) | +121 |

The candidate reached first pixels sooner in 9 of 15 fresh pairs and 2 of 15 repeat
pairs. A paired change is the median of same-round differences. First sign-in pixels
can precede the first settled field positions. Use the separate stable-geometry probe
to measure that milestone. The ranges overlap in this first batch.

#### Repeat launch tail

An independent block sampled 100 repeat launches per build. Five fresh Xvfb and
D-Bus sessions each ran one warmup per build, then 20 alternating pairs. The starting
build alternated between sessions. Both packages and the first-visible-pixels observer
matched the initial batch. All 200 measured launches exited cleanly.

| Repeat sign-in pixels | Parent, ms | Candidate, ms |
| --- | ---: | ---: |
| p50 | 523.92 | 498.02 |
| Nearest-rank p95 | 820.83 | 823.19 |
| Range | 379.60–997.08 | 363.23–1,110.71 |

The paired median changed by −12.90 ms, with the candidate earlier in 54 of 100
pairs. Session medians moved in both directions. The initial 15-pair repeat slowdown
did not reproduce across the larger block. The larger block shows a similar tail for
these Linux software-rendered builds. The hardware design targets require their own
reference machine and renderer measurements.

#### First stable sign-in fields

A separate repeat launch probe tracked the two native sign-in fields for 500 ms
after it first saw them. It compared their observed bounds with the final stable
geometry at a two-pixel tolerance. A stable suffix needed at least four matching
samples spanning 75 ms. The result timestamps the start of that suffix. The observer
sent no input. All eight alternating pairs were usable and all 16 processes exited
cleanly.

| Repeat sign-in milestone | Parent median (range), ms | Candidate median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Spawn to stable fields | 650 (481–986) | 465 (389–785) | −161 |
| First fields to stable fields | 31 (23–113) | 0 (0–0) | −31 |

The field bounds moved by about 35 px in each parent launch. The candidate's first
observed fields matched their settled bounds in every launch. The candidate reached
stable fields sooner in six of eight pairs. Both warmups needed forced cleanup.
Excluding the first measured pair after those warmups, the geometry result held in
all seven pairs and the paired stable-time change was −102 ms. Wall times overlap.
A zero field shift delay means the first observed fields already matched the final
geometry at the observer's sampling cadence.

#### Click on the first visible field

In the packaged candidate, a separate XTest observer clicked the center of the
first detected empty password field within 0.35 ms of its sampling timestamp. The
click sent no text or form submission. The fresh qualification and all five repeat
samples showed the focus ring on the intended field. Click to focus pixels on repeat
was 180 ms median (133–251 ms). All measured launches exited cleanly. The warmup
needed forced cleanup after the response. The earlier prototype comparison sent its
successful click after geometry settled, so that timestamp describes a different
stimulus. The current probe verifies the initial hit target and measures software
X11 response. It leaves hardware input latency and p95 unmeasured.

#### Process and package resource use

In five separate paired memory launches per profile, all 20 processes exited cleanly.
The observer read Linux `smaps_rollup` at first content and one second later. RSS
includes the software driver's process allocations.

| Profile and measurement | Parent median, KiB | Candidate median, KiB | Median paired change, KiB |
| --- | ---: | ---: | ---: |
| Fresh RSS at first content | 276,092 | 271,604 | −4,772 |
| Fresh RSS one second later | 284,992 | 280,784 | −4,680 |
| Repeat RSS at first content | 263,172 | 258,000 | −5,172 |
| Repeat RSS one second later | 272,656 | 267,560 | −5,148 |

Repeat process CPU between first content and the one-second sample was 46 (42–49)
ticks on the parent and 29 (27–32) on the candidate. A tick was 10 ms. Fresh ranges
overlapped. The interval starts at each build's own first-content timestamp, so these
values leave CPU before first content and sustained idle behavior unmeasured.

| Package measurement | Parent, bytes | Candidate, bytes | Change, bytes |
| --- | ---: | ---: | ---: |
| Executable | 107,272,680 | 107,273,704 | +1,024 |
| Installed payload | 112,000,982 | 112,002,006 | +1,024 |
| Compressed Debian package | 57,039,360 | 57,042,440 | +3,080 |

### Minimal release renderer control on Linux

Compared the packaged `afe73481` prototype with a small eframe window using the same
Rust 1.99.1 release profile, Serein GPU adapter selection, transparent 1120 × 760 window
and FIFO surface. The control draws one working button. Both processes used private offline
Xvfb, 1× scaling, llvmpipe Vulkan and the same 4 ms pixel observer. There was one warmup
per build and profile and 15 alternating measured pairs per profile. Fresh isolated the
profile and Mesa cache. Repeat reused both. All 60 launches exited cleanly.

| Time to visible content | Control median (range), ms | Serein prototype median (range), ms | Median paired gap, ms |
| --- | ---: | ---: | ---: |
| Fresh | 340 (304–616) | 705 (558–875) | 324 |
| Repeat | 123 (108–321) | 439 (321–641) | 314 |

The paired gap was already 292 ms fresh and 314 ms repeat at the first observed window
map. The control has a 17,732,176-byte executable, three direct shared-library dependencies,
and a simpler first view. The package has a 107,296,232-byte executable and 17 direct
shared-library dependencies. Investigate process and native initialization before changing
the renderer. These are local software-rendered comparisons. The gap combines binary loading,
app setup and first-frame work. Physical GPU timing and p95 remain unmeasured.

Three additional interleaved repeat pairs captured glibc 2.41 startup statistics from
each app's own process. Median glibc startup timer counts were 0.33 million cycles
for the control and 16.74 million for Serein. Relocation accounted for 0.13 million
and 14.64 million cycles, respectively. These are glibc's reported timer units. They
confirm extra linker work, and leave app-initiated library loads, native window and
GPU setup, and attribution to individual libraries for separate measurements.

### Resident tray Show on offline X11

Used the exact packaged `afe73481` prototype, whose tray path matches `5340c069`.
A private StatusNotifier watcher registered the real Serein item and verified its Show
and Quit menu. The observer sent native Close, confirmed the window was unviewable for
350 ms, invoked the tray activation, and observed the same PID and X11 window ID remap.
There was one warmup and five measured launches in offline Xvfb on llvmpipe Vulkan.
Every measured process exited through the real tray Quit action.

| Time from StatusNotifier `Activate` | Median, ms | Range, ms |
| --- | ---: | ---: |
| X11 window remapped | 114.3 | 112.7–123.6 |
| First varied sign-in pixels | 136.8 | 135.6–150.7 |

The window and renderer were already resident. The result includes X11 observation and
can include pixels retained by the server. Remap alone exceeded the 90 ms resident
target in every sample. Eframe's hidden repaint delay is on the existing X11 activation
path. Measure a dedicated wake from the tray event to main-loop logic while preserving
background throttling. Cold tray, panel focus, scanout and p95 are separate measurements.
[Tray lifecycle design](startup-design.md#tray-lifecycle-and-failure-handling)

#### Immediate X11 tray activation

The candidate routes a coalesced Show through the native event loop for immediate
processing. Ordinary hidden-window repaint requests still use the 100 ms delay.
The comparison uses the exact packaged runtime from the parent branch (`9a1e9b34`)
and the candidate (`47af0a19`). Both are standard voice-enabled Debian packages.
On the same offline Debian 13 VM (Xeon Platinum 8573C, 4 vCPU quota, 16 GiB RAM),
the observer runs signed-out X11 on Xvfb at 1440×1000 and scale 1 with llvmpipe
Vulkan. It dispatches Show through a private StatusNotifier host and polls X11
every 4 ms. The alternated trials use separate warmed profiles and one warmup
per build. The short and long cohorts wait 350 ms and 4 s after X11 reports the
window unviewable, respectively.

| After a 350 ms hide | Parent median (range), ms | Candidate median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Show to X11 remap | 113.6 (110.6–135.9) | 12.2 (10.9–69.9) | −101.4 |
| Show to varied sign-in pixels | 137.5 (133.4–159.2) | 36.8 (32.1–96.7) | −100.8 |
| Process CPU during the hidden interval | 95 (80–170) | 90 (80–130) | 0 |

Both Show milestones improved in all 12 pairs. X11 can display retained content
on remap. The visible-pixel measure describes what the observer sees. Fresh
WGPU presentation, desktop-panel focus and hardware p95 need separate tests.

| After a four-second hide, three pairs | Parent median (range) | Candidate median (range) | Median paired change |
| --- | ---: | ---: | ---: |
| Show to X11 remap, ms | 148.9 (120.8–150.3) | 54.7 (29.8–59.8) | −95.6 |
| Show to varied sign-in pixels, ms | 166.3 (147.7–179.9) | 96.4 (68.5–106.4) | −83.5 |
| Process CPU during the hidden interval, ms | 110 (100–150) | 120 (100–190) | +10 |
| Process RSS before Show, KiB | 268,952 (266,476–269,028) | 267,600 (267,184–267,764) | −1,188 |
| Process PSS before Show, KiB | 240,516 (237,989–240,529) | 239,164 (238,732–239,284) | −1,245 |

The long-hide cohort reads `/proc/<pid>/smaps_rollup` immediately before Show.
PSS apportions shared resident pages to Serein. The readings cover the Serein
process. Xvfb, D-Bus, and dedicated GPU memory require separate measurements.
Hidden CPU uses 10 ms process ticks and includes tail work from closing the
window. The overlap and small sample leave the differences in hidden CPU and
memory inconclusive.

A separate interaction trial clicked Settings after Show and verified the new
navigation in the app and X11 screen. The observer dismissed Settings, closed
the window, and sent Show as soon as X11 confirmed the unmap. Both builds
kept the same process and window, opened Settings again, and quit through
the tray. The initial rapid remap was 31.0 ms on the parent and 92.8 ms on the
candidate. A fresh block of 12 rapid pairs passed all interaction checks and
had a median paired remap change of +0.4 ms with a range of −11.3 to +9.9 ms.
Median time to pixels was 41.6 ms on the parent and 40.8 ms on the candidate,
with ranges of 38.6–55.2 ms and 38.2–82.0 ms. The repeat block places the
median difference within the 4 ms polling interval. Its slower candidate
samples leave the rapid-reopen tail unresolved.

| Standard Debian build | Parent | Candidate | Change |
| --- | ---: | ---: | ---: |
| Executable bytes | 107,273,704 | 107,277,672 | +3,968 |
| Full package installed size, KiB | 109,581 | 109,585 | +4 |
| Compressed package bytes | 57,042,440 | 57,033,308 | −9,132 |

### Foreground launch and X11 global shortcuts

The X11 hotkey worker in `global-hotkey` 0.8.0 serviced one command per poll
and then slept for 50 ms. The desktop waits for each configured registration
during window setup. On a signed-out default profile, it registers both the
mute and deafen shortcuts before first paint.

A diagnostic ran the same standard Debian executable (`47af0a19`) with only
`keybinds.global_enabled` changed. The two profiles were cloned from the same
warmed no-account SQLite store and graphics cache. On 1× Xvfb with llvmpipe
Vulkan, a private D-Bus session and networking disabled, each setting had one
warmup before 20 alternating repeat-launch pairs. All 40 measured runs exited
cleanly. Both settings still created the system hotkey manager.

| Milestone | Global shortcuts on, median (range), ms | Global shortcuts off, median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| X11 window mapped | 432.9 (383.8–768.1) | 353.5 (308.8–583.7) | −88.2 |
| Varied sign-in pixels | 439.5 (390.3–782.3) | 407.6 (353.1–653.6) | −49.7 |

Disabling the shortcut path shifted the visible milestone earlier in 14 of 20
pairs. The ranges overlap. Compare the worker directly and keep global shortcuts
enabled when comparing production packages.

The patched worker lets requests wake the existing thread immediately. At each
wake it handles pending X11 events before the command. In a separate direct
test, each fresh process created the manager and registered Ctrl+Shift+M and
Ctrl+Shift+D in order. The split builds used the same Rust release profile, a
private Xvfb session, one warmup each and 12 alternating pairs.

| Direct worker measurement | Registry median (range), ms | Patched median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Second registration | 50.392 (50.256–54.468) | 0.230 (0.185–3.747) | −50.147 |
| Manager and both registrations | 72.752 (69.501–94.819) | 23.058 (20.330–91.738) | −50.188 |

The second registration improved in every pair. Manager setup and the first
registration are included in the total. Both builds also passed a competing
grab, XTest press and release during an 820 ms repeat-enabled hold, suppression
of events after unregister, and shortcut reacquisition after dropping the
owner. Reassignment before drop used a checked round trip on the same
connection to flush the buffered ungrab.

#### Packaged launch comparison

Standard voice-enabled Debian packages from `47af0a19` and `e179555e` ran
signed out with global shortcuts enabled. The offline Xeon Platinum 8573C VM
had a four-CPU quota and 16 GiB RAM. Both builds used llvmpipe Vulkan on a
1440×1000 Xvfb display at scale 1. The 4 ms X11 observer timed each launch
from process dispatch. It observed every measured launch exit cleanly.

Five independent display and D-Bus sessions measured 20 repeat pairs each.
Within each session, each build warmed its own profile once before the samples.
Launch order alternated within and between sessions. The p95 is nearest rank
over 100 launches per build.

| Repeat milestone | Parent p50 / p95, ms | Patched p50 / p95, ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| X11 window mapped | 465.7 / 756.3 | 375.2 / 691.8 | −78.9 |
| Varied sign-in pixels | 469.4 / 777.9 | 416.7 / 702.3 | −52.2 |

The visible milestone occurred earlier in 71 of 100 pairs. All five sessions
had a lower paired median, ranging from −152.1 to −26.7 ms. Observed visible
times ranged from 372.9 to 1,039.2 ms on the parent and 322.8 to 744.2 ms on the
candidate. These results describe software rendering on this VM.

Fresh trials created a new no-account profile and driver cache for each
launch. The OS page cache stayed warm. In the 15-pair cohort, visible p50 was
738.7 ms on the parent and 759.4 ms on the candidate, with a median paired
change of −64.7 ms. In a second 20-pair cohort it was 883.5 ms and 876.3 ms,
with a median paired change of −23.5 ms. The candidate was faster in 19 of
35 pairs, with mixed cohort medians and overlapping ranges.

In separate process-memory cohorts of five and eight repeat pairs, median
PSS at first content was 5,403 KiB higher and 2,379 KiB lower, respectively.
One second later the differences were −297 and +219 KiB. The median
process CPU over that second moved by −5 and +4 ticks (10 ms per tick).
These samples leave the first-frame and settled differences inconclusive.
PSS apportions shared resident pages to the Serein process. Dedicated GPU
memory and the X11 server's memory are outside the sample.

| Standard Debian build | Parent | Patched | Change |
| --- | ---: | ---: | ---: |
| Executable bytes | 107,277,672 | 107,283,048 | +5,376 |
| Full package installed size, KiB | 109,585 | 109,592 | +7 |
| Compressed package bytes | 57,033,308 | 57,037,116 | +3,808 |

### What the baseline does at launch

| Milestone | Work on the critical path |
| --- | --- |
| Before the first window | Claim the single instance, open and initialize SQLite including schema work, read launch preferences and the saved Discord device profile, set up diagnostics, decode the embedded window icon, and create the native window and WGPU renderer. Keep the saved renderer, transparency, window placement and launch mode available in time for the first window. |
| Before the first visible content | Install base fonts, prepare and queue the Twemoji and icon textures, build the two video conversion pipelines, start the app workers, and run the first UI pass. A configured extension start screen can read and decode its saved layout in that pass. WGPU uploads queued textures before presenting. |
| Saved session restore | Native credentials and subsequent SQLite reads already run on separate workers. The connection can wait for the saved presence read on the SQLite worker. The preview queues its snapshot and then the selected channel's history on that same worker. The Gateway worker can also await build, device registration, and presence requests before identifying. |
| Live chat | The Gateway worker prepares READY and its permissions in the background. The UI applies it, reconciles navigation, runs the cached-to-live handoff, and requests recent history. READY, first authenticated paint, and first fresh messages can occur at different times. |

The startup code is in [`main.rs`](../apps/desktop/src/main.rs), the restore path in
[`sign_in.rs`](../apps/desktop/src/sign_in.rs), and transport setup in
[`connection.rs`](../apps/desktop/src/connection.rs). The OS startup entry, registered games
read, G-SYNC probe, and later reads such as appearance and account roster already do their I/O
off the UI thread. They can still contend for resources during launch.

### Earlier changes and current overlap

- Large READY payloads became bounded and atomic. Later work decoded each guild once for
  permissions and made the first server-rail pass cheaper. The synthetic 9.5 MiB READY
  permission workload went from about 28 ms to 17–18 ms. [#155](https://github.com/ViceVerse-cz/Serein/pull/155) ·
  [#420](https://github.com/ViceVerse-cz/Serein/pull/420)
- CJK font inflation moved to first demand, and Twemoji decoding stopped using a separate
  full-size temporary pixel buffer. The current bundled atlas still decodes roughly 5.2 MB
  of PNG into 16.5 MB of pixels during app creation. An open upstream PR switches to
  demand-driven glyph textures. Another open PR changes how bundled icons are rasterized.
  [#349](https://github.com/ViceVerse-cz/Serein/pull/349) ·
  [#1040](https://github.com/ViceVerse-cz/Serein/pull/1040) ·
  [#963](https://github.com/ViceVerse-cz/Serein/pull/963)
- Chat rows were stabilized on their first paint, and static skeletons replaced several
  spinner or image flashes. Extensions can supply their own start screens. Those changes
  address different parts of what people see during startup and navigation.
  [#395](https://github.com/ViceVerse-cz/Serein/pull/395) ·
  [#1007](https://github.com/ViceVerse-cz/Serein/pull/1007) ·
  [#1042](https://github.com/ViceVerse-cz/Serein/pull/1042)
- Desktop now hands its SQLite connection to the cache worker, eliminating a second open.
  Gateway setup uses a fixed endpoint and READY supplies identity, removing two earlier
  REST dependencies. The remaining startup requests are conditional.
  [Store reuse](https://github.com/ViceVerse-cz/Serein/commit/bedb57c0c8def62e736e8a4f03e0a5d248b22126) ·
  [Gateway startup](https://github.com/ViceVerse-cz/Serein/commit/f72925e507db5aaf706f4db040cdbc8d627a0ab6)

### Follow-up experiments

1. Time database initialization, renderer creation, asset preparation, and submission of
   the first frame separately. Record when the shell first responds to input and look at
   frame times through the handoff to live chat. Compare a new profile, an existing
   current-schema profile, and a migration separately. Test slower or locked storage while
   preserving the initial window preferences. Video conversion is a good first candidate to
   move behind media demand. Compare the first video frame too, to check for a playback hitch.
2. For the rewritten restore flow, test delayed last-conversation reads, manual navigation
   while the read is pending, and missing or inaccessible selections. Profile the time from
   worker READY to UI READY, selected channel and fresh history separately. If the selection
   read competes with settings-only work, investigate deferring those reads.
3. Measure large READY separately from launch and profile UI sorting and remaining network
   waits before changing their ordering.

### Video conversion setup on offline Linux

A temporary probe on `ec816a1a` timed the existing `gpu_video::install` call in a native
`--demo` build with pinned Rust 1.99.1. The converter and eager call are also present at the
rewritten PR head `5340c069`. The probe alternated the normal launch call, the same call about
700 ms after the first UI pass, and a no-op control at each point. It recorded the change in
process RSS and private dirty memory from `/proc/self/smaps_rollup` immediately across the
call. The renderer was Vulkan on llvmpipe with no hardware GPU.

| Fresh XDG cache, 15 trials each | Setup median (range) | Median RSS delta | Median private dirty delta |
| --- | ---: | ---: | ---: |
| At the existing launch call | 12.884 ms (12.267–20.107) | +1,788 KiB | +1,400 KiB |
| At launch, no-op control | <0.01 ms | +64 KiB | 0 KiB |
| After the first UI pass | 8.471 ms (6.988–11.335) | +388 KiB | +388 KiB |
| After the first UI pass, no-op control | <0.01 ms | 0 KiB | 0 KiB |

| Reused XDG cache, 10 trials each | Setup median (range) | Median RSS delta | Median private dirty delta |
| --- | ---: | ---: | ---: |
| At the existing launch call | 9.098 ms (8.390–11.278) | +1,102 KiB | +776 KiB |
| At launch, no-op control | <0.01 ms | +32 KiB | 0 KiB |
| After the first UI pass | 8.071 ms (7.228–11.187) | +618 KiB | +618 KiB |
| After the first UI pass, no-op control | <0.01 ms | 0 KiB | 0 KiB |

The fresh trials used new XDG cache directories. The reused trials shared a cache directory
within each mode after one warmup. Mesa populated the cache. The probe did not attribute
individual shader hits.

The wgpu allocator reported zero change in its managed allocated and reserved bytes. The
RSS sample includes process code and driver allocations. Dedicated hardware VRAM, transient
memory peaks and decoded video textures are outside this sample. The native first-content
timing ranges overlapped. This component measurement leaves release behavior and time from
media request to first video frame open.

#### Demand-driven YUV preparation

The candidate creates lightweight video handles with the desktop renderer.
Explicit Play starts one shader setup alongside the planar attachment decoder
on Linux and macOS. A watched stream or remote camera can also start it when
an eligible planar picture is available in the visible window. RGBA upload is
immediate. Until preparation finishes, the decoder's existing bounded slot
holds the latest eligible picture. The same renderer reuses its ready shaders
after a tray hide, and the worker asks for a repaint when it publishes a result.

The playback comparison used locked Rust 1.99.1 fat-LTO builds of `ccf289d8`
and `a873fe3c` with `--features demo`, on offline 1× Xvfb with llvmpipe Vulkan.
Both original demos stopped playback in the private audio fixture. The visual
test used a video-only stream copy of the synthetic MOV,
padded to the original length with a free atom. All 72 video packet hashes,
timestamps and durations matched. Each diagnostic binary differed from its
original release demo only in the embedded fixture bytes.

An XTest Play click starts the ordinary decoder and texture upload. The observer
polls X11 every 4 ms for the first visible source-confirmed motion. The poster
and the first decoded picture can look identical. The result measures the first
distinguishable picture at 24 fps. Playback with audio and presentation on a
physical display need separate measurements.

| Play to first visible motion | Parent median (range), ms | Candidate median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Reused profile and driver cache, 20 pairs | 165.1 (120.5–374.5) | 155.0 (121.1–301.9) | +0.9 |
| Fresh profile and driver cache, 15 pairs | 230.6 (148.9–380.8) | 222.3 (147.8–333.8) | −1.1 |

Both cohorts had one warmup for each build and alternated their launch order.
Every measured run played the clip and exited cleanly. The sample ranges
overlap, and both paired medians are within the polling interval. The warmed
observer matched source frame 1 in 18 parent and 20 candidate trials.

A separate Linux syscall trace of the candidate observed zero shader workers
while displaying the idle poster and one after Play. Both processes exited
cleanly. Trace runs were excluded from the timing cohorts.

The normal voice-enabled Debian packages from `e179555e` and `a873fe3c` both
passed the 248-file package smoke check. The optimized demo and its silent
fixture were separate from these packages.

The first signed-out package comparison ran on Debian 13 with the same
offline 1× Xvfb and llvmpipe Vulkan renderer. The Xeon Platinum 8573C VM had a
four-CPU quota and 16 GiB of RAM. Each build had one warmup, then launch order
alternated. Fresh profiles and Mesa caches were new per launch, and repeat
profiles and caches were reused per build. The OS page cache was left intact.
The observer polled X11 every 4 ms. All 70 measured launches exited cleanly.

| Spawn to visible sign-in | Parent median (range), ms | Candidate median (range), ms | Median paired change, ms |
| --- | ---: | ---: | ---: |
| Repeat profile, 20 pairs | 446.1 (356.5–625.4) | 415.4 (352.8–686.2) | −56.0 |
| Fresh profile, 15 pairs | 728.9 (532.6–1,003.5) | 780.8 (650.1–1,097.6) | +41.6 |
| Fresh profile, fixed dark theme, 20 pairs | 744.8 (575.1–930.9) | 762.5 (563.3–1,196.3) | +61.9 |

The candidate showed sign-in sooner in 13 repeat and seven fresh pairs. The
fresh first captures included a dark parent and a light candidate while both
followed the desktop theme. This can change first-paint work and the frame
that the observer samples. A third session reversed the starting build and
used a private GSettings schema that always reported dark, matching egui's
fallback. All 40 runs presented dark sign-in and exited cleanly. The sampled
first crops matched pixel for pixel. The candidate was earlier in eight pairs.

Fresh window map to visible pixels took 8.9 ms median on the parent and
68.7 ms on the candidate in the first comparison, a median paired increase
of 59.7 ms across 15 pairs. With the fixed theme it took 7.8 ms on the parent
and 62.3 ms on the candidate, a median paired increase of 50.4 ms. The
candidate's interval increased in 19 of 20 fixed-theme pairs. The two builds
can differ in when X11 maps the window and when the first presented pixels
arrive. Instrument those events to locate the fresh-cache delay before treating
the candidate as a launch improvement.

Separate memory runs used eight pairs per profile with the default desktop
theme and eight per profile with the fixed dark theme. The observer read
process PSS at first sign-in pixels and one second later. All processes exited
cleanly. PSS includes the software renderer's allocations.

| Median paired change, candidate minus parent | PSS at first content | PSS one second later |
| --- | ---: | ---: |
| Default theme, fresh profile | +3,340 KiB | −1,447 KiB |
| Default theme, repeat profile | +2,319 KiB | −868 KiB |
| Fixed dark theme, fresh profile | +1,147 KiB | −853 KiB |
| Fixed dark theme, repeat profile | −730 KiB | −675 KiB |

The default-theme runs had lower one-second PSS in all 16 pairs. Under the
fixed theme it was lower in seven fresh and five repeat pairs. First-content
memory varied in both directions. Process CPU between first content and the
one-second sample changed by a paired median of 0 ms fresh and +160 ms repeat
under the fixed theme, at 10 ms accounting resolution. Each interval starts at
that run's own first content, so it can include a different share of work left
over from launch.

| Package measurement | Parent | Candidate | Change |
| --- | ---: | ---: | ---: |
| Executable | 107,283,048 bytes | 107,286,120 bytes | +3,072 bytes |
| Installed size | 109,592 KiB | 109,595 KiB | +3 KiB |
| Compressed Debian package | 57,037,116 bytes | 57,037,772 bytes | +656 bytes |

### When to load optional work

Serein has priority and admission rules within individual subsystems. The artwork worker
chooses viewer work ahead of queued inline work and caps concurrent decoding. An admitted
job can still occupy a slot. The rewritten PR limits classified background REST requests to
two of the four shared slots. Speculative reads decline work when capacity or a route is busy.
The desktop unwraps `Command::Prefetch` before it reaches that classification, so hover
prefetch still uses ordinary admission. SQLite uses one ordered worker and ordinary Tokio
background tasks have no application-wide admission rule. [Artwork](../apps/desktop/src/avatars.rs) ·
[Desktop dispatch](https://github.com/Neur0nz/Serein/blob/5340c069fc42616ddd01836c1cf27a1028ebfdd8/apps/desktop/src/connection.rs#L1325) ·
[REST reserve](https://github.com/Neur0nz/Serein/blob/5340c069fc42616ddd01836c1cf27a1028ebfdd8/crates/discord-api/src/lib.rs#L687)

Start optional media setup when playback or viewing is requested. Video decode and stream
negotiation already have worker paths, which gives shader creation a chance to complete while
the first picture is being prepared. Keep one setup in flight, publish the ready result without
blocking a paint pass, and measure input-to-first-video-frame. The RGBA upload path can bypass
YUV shaders, and NV12 and I420 can be prepared independently if measurements justify doing so.
[Video conversion](../apps/desktop/src/gpu_video.rs) · [Inline play](../apps/desktop/src/video.rs) ·
[Watch stream](../apps/desktop/src/watch.rs)

For a task that actually benefits from post-launch warmup, admit one bounded job only after
the relevant foreground work settles. Coalesce duplicate requests and let an explicit user
action adopt the running result or take priority over queued work. Avoid starting another unit
when the app is actively rendering or servicing interaction. A worker can still contend for
CPU, disk, or the graphics driver once a job starts, so measure responsiveness during warmup.

| Work | When to load it | Reuse across launches |
| --- | --- | --- |
| Window settings and local storage | Read the settings that choose the renderer, placement and initial appearance early. Profile migration and current-schema opens separately before moving the remaining work. | Use the existing saved preferences. Measure skipping repeat migration checks before adding a second source of truth. |
| Fonts, emoji and icons | Keep first-view assets available. CJK and installed-font enumeration already load on demand. Measure emoji after upstream #1040. | Generate fixed indexes, icon assets and common size variants at build time if they are measured bottlenecks. Keep fonts and GPU textures tied to the active scale and renderer. |
| Extensions | Keep the granted startup screen available early when configured. Start work specific to other extension views when they are used. | The granted start screen already has a bounded saved descriptor. Profile read and decode before changing its representation, and preserve disable, revoke and removal behavior. |
| Activity, updates and settings | Investigate deferring automatic update checks until after startup, and loading OS autostart status, GIF favorites and registered games when their settings or features actually need them. Explicit actions still need an immediate path. | Continue using existing settings and approved cache formats. Avoid writing an extra copy just to move a small read. |
| Artwork and session navigation | Prioritize artwork for the visible viewport and history for the selected channel. Defer optional scans or hydration for superseded selections. | Use the existing bounded artwork cache and the rewritten PR's account-scoped last-conversation record. |

The renderer and video paths currently pass `cache: None` to wgpu. wgpu 30 exposes a
persisted pipeline cache only on Vulkan. First measure the installed driver's cold and warm
cache with the same adapter. Consider an application cache only if it adds a measurable gain
and can be rebuilt on miss. Any retained blob should be generated locally, versioned for the
adapter, driver and pipeline, bounded, and written atomically off the launch path.
[wgpu pipeline cache](https://docs.rs/wgpu/30.0.1/wgpu/struct.PipelineCache.html) ·
[Mesa driver cache](https://docs.mesa3d.org/envvars.html#mesa-shader-cache-dir)

## Voice connection ping

Compared `5a11f95b` with the voice ping change using locked Rust 1.99.1 release builds
with `--features demo`. Windows x64 build 26300.9711, Ryzen 7 7800X3D, about 32 GiB RAM,
DX12 and the same 1120 × 760 demo window. Launch `serein.exe --demo --demo-voice`,
leave the connection popup closed with the pointer off the controls, warm up for eight
seconds and sample `WorkingSet64` about every 100 ms for twelve seconds. CPU is process
CPU time divided by elapsed time, as a percentage of one logical core. Settled working
set averages the final five samples. Private bytes are read at the end of the run.
Two runs per revision, in baseline/change/change/baseline order. No child processes
were present at the end of the runs.

| Metric | Baseline | Voice ping |
| --- | ---: | ---: |
| Demo executable | 100,484,608 B | 100,369,920 B |
| Idle CPU, range | 0.384–0.388% | 0.258–0.647% |
| Peak working set, range | 145.81–158.10 MiB | 142.33–142.36 MiB |
| Settled working set, range | 145.81–158.09 MiB | 142.33–142.36 MiB |
| Settled private bytes, range | 178.90–194.93 MiB | 178.56–180.00 MiB |

The preliminary run, before controlling pointer position, showed the working set moving
in the opposite direction. The short samples do not establish a memory or CPU improvement.
The fixture renders synthetic RTT. Actual call CPU, GPU allocations and frame timing were
not measured. The transport samples existing ACKs without scheduling an extra probe. The
desktop keeps up to 24 millisecond values per connection.

Recent synthetic/offline measurements are workload-specific. They do not establish live Discord
performance, universal device results or application-wide memory bounds. The raw PR screenshot,
log and per-run evidence archive has been removed; the summaries below retain the useful results.

## DeepFilterNet in plain Rust — November 20, 2026

`crates/deepfilternet` runs the DeepFilterNet3 weights without an inference runtime. It
replaces the libDF/tract 0.22.5 runtime that PR #1085 vendored, and was compared with that
runtime in one test binary before the runtime was removed. Apple M5 Pro, 24 GB, macOS
27.2.1, Homebrew Rust 1.100 instead of the pinned 1.99.1, dev profile with both runtimes
at `opt-level = 3` as development builds run them. No release package was built.

A throwaway harness, not committed, timed each suppressor alone (no echo cancellation or
gain) over 1,000 10 ms hops of synthetic noise, harmonic "speech" and silence, three
rounds each. The worker runs two hops per 20 ms frame.

| Metric, median of 3 rounds | libDF on tract 0.22.5 | `crates/deepfilternet` |
| --- | ---: | ---: |
| Per-hop median | 0.965 ms | 0.179 ms |
| Per-hop p99 | 1.095 ms | 0.211 ms |
| Start (decode and optimize networks / build state) | 289 ms | 0.14 ms |

The port's slowest hop was 4.2 ms in the first round and 0.22 ms in later rounds: the first
hops page the embedded weights in. Two hops per frame are about 1.8% of real time.

Output parity over 9,000 hops (90 s) of synthetic noise, speech-like tones, clipping and
digital silence, largest per-sample difference from the tract runtime:

| Setting | Largest difference | Error energy / output energy |
| --- | ---: | ---: |
| High (no cap) | 3.6e-6 | 8.2e-14 |
| Low (12 dB cap) | 2.4e-6 | 4.5e-14 |
| Very high (post-filter) | 3.6e-5 | 7.1e-11 |

The post-filter row differs more because the port also filters the 24 kHz bin, which
libDF's four-bin loop skips. The network alone matched ONNX Runtime on the original model
files within 1.5e-6 over 60 frames of unit-scale random features. Sparse features 300 and
3,000 times larger, which force the filter decoder's direct path active, matched within
6.2e-6 and 1.2e-4 on coefficients as large as 6.7 and 86.

In `rust:1.99.1` Linux containers the crate's tests passed on arm64 and on x86_64 (emulated
by Rosetta, so its timing is not representative). Per-hop medians there were 0.18–0.20 ms
on arm64, and two minutes of digital silence ran at the same median as noise.

The weights add 8,535,484 bytes to every executable; the tract runtime had added
25,964,032 bytes to the Windows x64 release executable in PR #1085. The lockfile gains no
third-party package. Release executable and package sizes, process memory, whole-app CPU,
real x86 hardware, Windows, physical microphones, perceived quality and live calls are
unmeasured. PR #903's SI-SDR table was measured on the tract runtime and not re-run.

## G-SYNC steady refresh offer — November 10, 2026

Baseline `09bee5a` and this branch were built with the pinned Rust 1.99.1 toolchain and locked
dependencies on Windows 11 x64 (build 26200). Standard release packages use `cargo xtask package`
(voice included, demo and development data disabled), each revision from its own worktree.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 98,454,528 B | 98,438,144 B | -16,384 B (-0.017%) |
| `dist/` regular-file payload (233 files) | 102,828,189 B | 102,811,805 B | -16,384 B (-0.017%) |
| ZIP of `dist/` (`7z a -tzip -mx=9`) | 53,784,480 B | 53,791,915 B | +7,435 B (+0.014%) |

The executable is the only `dist/` file that differs. The smaller executable and larger archive
are one build each and are not an improvement or a regression.

The G-SYNC check ran on an RTX PRO 6000 (driver 32.0.16.2114) driving one 3440×1440 240 Hz
display with G-SYNC on. Three concurrent checks in a debug build each reported within 27–28 ms
of being started, including the worker thread, loading `nvapi64.dll` and unloading it again.
In a debug `--features demo` build temporarily changed to run the check, the dialog appeared and
`nvapi64.dll` was no longer among the process modules 8 s after launch. The check runs once per
launch until the offer is answered or the setting is on, and never afterwards. Demo fixtures skip
it, so their idle CPU and memory were not sampled; release-build timing, a display with G-SYNC
off and a computer without an NVIDIA driver were not measured.

## Discord message toolbar and safe extension ports — November 8, 2026

Baseline `21f8870b` and this branch were built with pinned Rust 1.98.1 and locked dependencies on
Linux x86-64 (kernel 7.3.1-1-cachyos), Ryzen 9 9900X with 30 GiB RAM and an NVIDIA RTX 4070 using
Vulkan. The host display was X11 at 3840×2160 with Xft.dpi 192. Package measurements use the standard
voice-enabled `cargo xtask package --format dir` method on separate worktrees. The host has no `dpkg`,
so the default `.deb` command failed for that external reason; the supported directory package was
used for both revisions. Compression uses the same Python `tarfile` gzip method and sorted source order.

| Metric / method | Baseline `21f8870b` | After this branch | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 95,117,576 B | 95,126,280 B | +8,704 B (+0.009%) |
| Regular-file payload, 477 files each | 199,279,452 B | 199,298,284 B | +18,832 B (+0.009%) |
| Compressed package (`tar.gz`) | 113,557,397 B | 113,566,452 B | +9,055 B (+0.008%) |

The native `profile_preview` interactive fixture was built in release `--features demo` mode. The
identical example source was copied into the baseline worktree solely to produce the same close-
settings capture path; no baseline production code was edited. Baseline runs have no hover. Current
runs add `--toolbar-hover=normal`, which injects pointer movement and `ModifiersChanged` through
`RawInput` every frame. Both use `--demo --page=chat --interactive --close-settings` at 1120×760
logical pixels and `chat_demo_state()` with no account, network, voice call or helper process.

Three runs per revision alternated baseline/current order. Each run had a 15-second warmup followed
by 20 one-second samples of `/proc/<pid>/stat` and `/proc/<pid>/status`. Reported CPU sums user and
system ticks converted to a percentage of one logical core for the actual sample interval. Memory is
resident-set size (VmRSS in KiB), with main and helper processes summed separately when present.
No helpers existed, so combined equals main-process RSS. Peak uses each run's maximum; settled is
its final sample. No cargo build or source edit ran during sampling.

| Runtime metric, three runs | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Idle CPU, median | 9.396% of one core | 9.696% of one core | +0.300 percentage points (+3.19% relative) |
| Peak combined RSS, maximum | 235,040 KiB | 234,244 KiB | −796 KiB (−0.34%) |
| Settled combined RSS, median | 234,416 KiB | 233,768 KiB | −648 KiB (−0.28%) |

Per-run values and package figures are retained in
[the measurement record](pr-evidence/discord-message-toolbar/measurements.json), and both hover
images and their reproduction commands are in
[the capture guide](pr-evidence/discord-message-toolbar/README.md). The fixture intentionally
represents missing-hover versus active-hover, so the CPU delta is not an isolated implementation
regression estimate. Small memory differences are treated as noise, not an improvement. Frame time,
startup latency and live Discord performance were not measured.

## High-resolution attachment sources — November 6, 2026

Offline comparison against `77bdddae` on Ubuntu 24.04.5 x86-64, an Intel i5-1235U,
Rust 1.98.1 and locked dependencies. A standalone development wrapper compiled the production
`platform::video` module and opened the same original one-frame 7680×4320 H.264 MOV on both
revisions. The wrapper only checked decoder startup, output dimensions and frame validity;
it did not run the desktop renderer or contact Discord.

| Synthetic workload / output | Baseline | After |
| --- | --- | --- |
| Open the 8K source | Rejected with `TOO_LARGE` | Decodes at 3840×2160 |
| Copied NV12 picture | No picture produced | 12,441,600 B |

Generate that source with development-only FFmpeg 6.1.1:

```sh
ffmpeg -f lavfi -i color=c=blue:s=7680x4320:r=1 -frames:v 1 -c:v libx264 -threads 2 -preset ultrafast -crf 30 -pix_fmt yuv420p -movflags +faststart video-8k.mov
```

The Linux regression also decodes the committed 4096×64 fixture to 3840×60 and scales an
oversized rotated raw source to 1920×3840. The optional FFmpeg filter was exercised separately:
4096×64 becomes 3840×60, and 4096×4096 becomes 2880×2880. Each is one functional run,
not a throughput measurement. Copied frames retain their existing pixel/byte ceilings;
OS codec allocations, including full-resolution input surfaces, are additional. No whole-app
RSS, CPU, GPU time, playback latency, sustained frame rate or native Windows/macOS runtime
measurement was collected. Cross-compilation is not evidence of native playback.

## Window placement save after a move — November 5, 2026

Baseline `2b593b1` (`main` after #893) and this branch were built with the pinned Rust 1.98.1
toolchain and locked dependencies on Windows 11 x64 (build 26200). Standard release packages use
`cargo xtask package` (voice included, demo and development data disabled), each from its own
worktree.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 92,905,472 B | 92,905,472 B | 0 B |
| `dist/` regular-file payload (233 files) | 97,265,351 B | 97,265,351 B | 0 B |
| ZIP of `dist/` (`7z a -tzip -mx=9`) | 52,172,759 B | 52,172,071 B | −688 B (−0.001%) |

Only `serein.exe` differs, at the same size; the ZIP difference is compression noise.

Debug builds with default features and an empty `SEREIN_DATA_DIR` show the sign-in screen, which
renders nothing while idle. After the window had been idle for 8 s, a script moved it with
`SetWindowPos` (no input, activation or z-order change) while PresentMon 2.6.0 recorded the next
4 s. The baseline presented no frame and kept the startup placement in `app_preferences`; this
branch presented three frames about 1.0 s after the move and saved the new position. An idle window
gains no frames, and while a placement is unsaved the wake adds at most one frame per 500 ms.
Interactive drags were not re-measured.

## Window drags on Windows — November 5, 2026

Review follow-up: the measurements below describe the original `1f664628` implementation.
The revised adapter schedules a delayed 500 ms repaint on Windows moves to ensure a final
placement/monitor sample, and placement saving now checks elapsed monotonic time. This can
produce occasional frames during a long drag; the original present counts do not measure
this revision. Fresh Windows drag/idle and package-size comparisons are unavailable on the
Ubuntu review host: no native Windows session or C linker (`cc`) is available, and both
`cargo xtask check` and `cargo xtask package` stop while building xtask dependencies.
No updated performance claim is made.

Baseline `d57433c` and this branch were built with the pinned Rust 1.98.1 toolchain and locked
dependencies on Windows 11 x64 (build 26200). Standard release packages use `cargo xtask package`
(voice included, demo and development data disabled), each built the same day from its own worktree.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 92,904,448 B | 92,904,960 B | +512 B (+0.0006%) |
| `dist/` regular-file payload (233 files) | 97,263,837 B | 97,264,814 B | +977 B (+0.001%) |
| ZIP of `dist/` (`7z a -tzip -mx=9`) | 52,175,667 B | 52,172,078 B | −3,589 B (−0.007%) |

Only `serein.exe` and the copied `THIRD_PARTY_NOTICES.md` (+465 B) differ; the smaller ZIP is
compression noise.

Drags used release `--features demo` builds of both revisions on a Ryzen 5 7600X with 32 GB of
RAM, an RTX 5060 Ti (DX12, driver 32.0.16.1742) and a 3840×2160 120 Hz display at 100% scale. A
script placed the 1120×760 window at (400, 300), focused it, waited 12 s, pressed the primary
button 14 px into the title strip and sent absolute `SendInput` moves to the right: 300 steps of
3 px every 4 ms (750 px/s) or 200 steps of 8 px every 2 ms (4,000 px/s). After each step it read
`GetWindowRect`. Lag is how far the window trailed the cursor at that sample; position updates
count samples at which the window had moved. PresentMon 2.6.0 counted presents between the first
and last move. Character Map, a native Win32 window, is the reference. Runs alternated between
revisions. Two disturbed runs were repeated: one window lost focus before the press, and one was
sampled before it had come to rest.

| Drag workload | Lag mean / p95 / max | Position updates | Longest gap | Presents during move |
| --- | ---: | ---: | ---: | ---: |
| Character Map, 750 px/s (1 run) | 0.1 / 0 / 3 px | 297 / 300 | 10.3 ms | — |
| Baseline `--demo`, 750 px/s (3 runs) | 2.0–2.4 / 6 / 6–18 px | 140–144 / 300 | 12–28 ms | 139–145 |
| After `--demo`, 750 px/s (3 runs) | 0 / 0 / 0 px | 300 / 300 | 4.1–4.2 ms | 2–3 |
| Character Map, 4,000 px/s (1 run) | 0 / 0 / 0 px | 200 / 200 | 2.1 ms | — |
| Baseline `--demo`, 4,000 px/s (3 runs) | 14.0–15.1 / 32 / 32–40 px | 49 / 200 | 10–12 ms | 48 |
| After `--demo`, 4,000 px/s (3 runs) | 0.6–2.0 / 8 / 8–16 px | 178–192 / 200 | 4.0–6.0 ms | 1 |
| Baseline sign-in screen, 750 px/s (2 runs) | 1.9–2.2 / 6 / 6–24 px | 141–144 / 300 | 12.1–34.7 ms | 140–144 |
| After sign-in screen, 750 px/s (2 runs) | 0 / 0 / 0 px | 300 / 300 | 4.1 ms | 0 |

The baseline presented about 120 frames/s for the whole drag, and the window moved at most once
per frame, so it trailed the cursor by up to 40 px. After the change the window moves with each
input sample, as Character Map does; the remaining presents are the composer's 500 ms caret blink.
At 4,000 px/s it still trails some samples by one 8 px step. The sign-in runs use the same binaries
without `--demo` and with an empty `SEREIN_DATA_DIR`, so they also save placements: every run stored
the final position (1300, 300) before the script ended the process about 2.5 s after the drag.

Process CPU during the 750 px/s drag fell from 0.42–0.50 s to 0.28–0.36 s, and it stayed at
0.13–0.17 s at 4,000 px/s. Working set after each drag was 175.9–177.2 MiB for both revisions.
Idle `--demo` runs, maximized and focused, waited 12 s and sampled 20 s (first 2 s discarded): both
revisions presented 2.00 frames/s at 500 ms intervals, with 0.08–0.15% of one core and a
207.9–208.1 MiB working set for the baseline (2 runs) and 0.08–0.39% and 208.2 MiB after (3 runs).
Four baseline idle samples that recorded user input were discarded.

Synthetic input arrives at an exact rate and the script reads window positions from another
process, so these numbers are not end-to-end display latency. Physical mice on other hardware,
other refresh rates and drags while the UI renders continuously (an animation or playing video)
were not measured; continuous rendering still paces a drag to the refresh rate. macOS and Linux
keep upstream egui-winit behavior and gain only the rested placement save and the cached minimum
size, which were not measured there.

## Steady refresh rate for VRR displays — November 5, 2026

Baseline `d57433c` and this branch were built with the pinned Rust 1.98.1 toolchain and locked
dependencies on Windows 11 x64 (build 26200). Standard release packages use `cargo xtask package`
(voice included, demo and development data disabled). Each revision was built from its own
worktree, and each executable was checked for the new localization key before measuring.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 92,904,448 B | 92,909,056 B | +4,608 B (+0.005%) |
| `dist/` regular-file payload (233 files) | 97,263,837 B | 97,268,445 B | +4,608 B (+0.005%) |
| ZIP of `dist/` (`7z a -tzip -mx=9`) | 52,175,667 B | 52,177,524 B | +1,857 B (+0.004%) |

All other `dist/` files are byte-identical.

Frame presentation used release `--features demo` builds of both revisions, sampled with
PresentMon 2.6.0 (`--v1_metrics`) on a Ryzen 5 7600X with 32 GB of RAM, an RTX 5060 Ti (DX12,
driver 32.0.16.1742) and a 3840×2160 120 Hz display at 100% scale. The default `--demo` fixture ran
maximized and focused, with the pointer parked on another display. Each run waited 12 s and then
sampled 20 s; the first 2 s of each capture are discarded. CPU is process CPU time as a percentage
of one logical core, and memory is the working set at the end of the sample. The steady-on runs
add `--demo-steady-refresh`, which starts the fixture with the setting on.

| Workload (3 runs each) | Presents/s | Interval p50 / p95 | CPU, one core | Working set |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 2.00 | 500.0 / 500.5–501.1 ms | 0–0.08% | 212.3–212.8 MiB |
| Steady refresh off | 2.00 | 500.0 / 500.7–500.8 ms | 0–0.15% | 212.5–213.2 MiB |
| Steady refresh on | 119.7 | 8.33 / 8.48–8.53 ms | 25.5–29.9% | 212.8–213.1 MiB |

Idle presents arrive every 500 ms, consistent with egui's text-cursor blink in the focused
composer, and DWM composes those frames. With the setting on, 99.7–99.9% of intervals fall within
±25% of the 8.33 ms refresh period, with two or three longer gaps per run (37–50 ms at most), and
PresentMon reports every frame as hardware independent flip. Each refresh runs a full UI pass, so
the cost grows with refresh rate and window size. A separate run that moved focus to the desktop
after 10 s stopped presenting within that second.

Every run above kept focus for the whole sample. Runs that lost focus or received input were
discarded and repeated; input detection was added for the final batch, and two of the
setting-off runs come from an earlier batch without it. PresentMon records presents and flips,
not the panel's refresh, so display-side VRR behavior is not measured here.

## Push-to-talk notification cues — October 29, 2026

Baseline `1b2f894` and this branch were built with the pinned Rust toolchain and locked
dependencies on Windows x64 (10.0.26300). Standard release packages use `cargo xtask package`
(voice included, demo and development-data disabled) with separate output directories.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 92,304,384 B | 92,312,576 B | +8,192 B (+0.009%) |
| `dist/` regular-file payload | 96,663,146 B | 96,671,773 B | +8,627 B (+0.009%) |
| ZIP of `dist/` (`7z a -mx=9`) | 51,902,771 B | 51,908,426 B | +5,655 B (+0.011%) |

The two byte-original MP3 cues add 3,319 B before packaging. Edge detection keeps one bool and
no cache. The PTT cues play through a second notification worker, so a press or release neither
cancels nor is cancelled by a message or ring cue. That worker starts lazily on the first PTT
cue and keeps one capacity-one channel plus its thread until its `Sounds` value is dropped with
the app; decoded audio is released after each cue. No CPU, RSS or latency measurement is
claimed, because the offline fixture joins no call and plays no cue. Debug `profile_preview`
screenshots from both revisions are in [the evidence folder](pr-evidence/ptt-sounds/).

## Incoming stream quality, pacing and recovery — October 22, 2026

Watched-stream reception now repairs/reorders bounded RTP gaps, preserves H.264 references
through brief native backpressure, and retains sender timestamps through audio playback
and decoded-video presentation. A single timing controller uses authenticated sender reports,
a bounded adaptive jitter target, measured decoder lead and an audio-device clock estimate.
No dependencies, resolution presets, notification audio or UI layouts change. The Linux
receive-only HEVC extension is described below.

October 25 package comparison: unchanged upstream `c2842aa` versus the streaming branch, pinned
Rust 1.98.1, Linux x86_64, `cargo xtask package --format arch` with voice and without demo.

| Package metric | Upstream baseline | Streaming candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,650,248 B | 92,802,184 B | +151,936 B (+0.164%) |
| Installed payload (245 files) | 97,348,681 B | 97,500,617 B | +151,936 B (+0.156%) |
| Arch archive | 54,376,694 B | 54,477,570 B | +100,876 B (+0.186%) |

Installed bytes sum regular archive files under `usr/`, excluding package metadata.
Both packages contain 245 installed files. The October 25 gate passed 1,321 tests, strict
Clippy, formatting, application and policy checks; its native Arch package smoke also passed.

### Linux HEVC reception extension

The actual preceding streaming build is `e9dae84`, verified by SHA-256 and preserved
before editing. It negotiates only H.264. The extension offers receive-only H.265 when
the installed parser/decoder and bounded pipeline initialize, keeps H.264 fallback and
handles session codec updates. Existing encoded/decoded queue limits and A/V timing apply.
No new dependency, media cache or audio-processing change is introduced.

| Standard Arch package / same pinned toolchain | Before `e9dae84` | HEVC candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,751,880 B | 92,767,368 B | +15,488 B (+0.017%) |
| Installed payload | 97,450,313 B | 97,465,801 B | +15,488 B (+0.016%) |
| Arch archive | 54,448,309 B | 54,449,636 B | +1,327 B (+0.002%) |

Offline native checks use an eight-second 1280×534/60 FFmpeg `testsrc2` fixture,
libx265 ultrafast, 4.1 Mbps, four pool threads/two frame threads, 8-bit 4:2:0,
AUDs/repeated parameter sets, key interval 60, no B frames or scene cuts. The production
clock/audio/playback/native decoder runs through GStreamer/VA-API on the same RX 6900 XT,
with a simulated 73 ms device delay. Four single functional cases cover a temporary
+200 ms video timestamp phase, backwards audio epoch, 500 ms audio loss and 400 ms delayed
audio startup. Each delivers all 480 pictures with reference-identical luma CRCs and
correct presentation tags, zero decoder errors/stale/admission rejections, and zero video
queue resets/drops. The final two input seconds retain all 100 audible frames without
concealment. Delayed audio acquisition has a 108 ms picture interval during startup.
These are functional checks, not physical output latency or a live FPS benchmark.

The existing H.264 timestamp-phase fixture still delivers all 479 expected pictures
(the H.264 parser retains one without EOS), matching reference luma and presentation
tags. Encrypted HEVC aggregation/fragmentation/RTX tests verify DAVE authentication and
keyframe recovery; malformed packet/SPS tests cover header disagreement, nested/truncated
aggregation, item/byte bounds, cropped dimensions and parameter-unit boundaries. Codec
negotiation tests retain H.264 senders and reject unoffered/unknown formats.

The old implementation cannot run an HEVC receive workload, so no codec speed comparison
is claimed. Native idle CPU/RSS and Windows/macOS HEVC support are unmeasured. The later owner
comparison is recorded in the UHD reception section below; it does not establish universal
perceptual-quality parity.

### Receive payload routing correction

The preceding `169bfdc` candidate incorrectly used the outgoing session selection as
an incoming payload filter. Its preserved 75.096-second owner capture has zero admitted
video packets/pictures and 3,587,520 decoded audio samples without concealment. That
capture did not count discarded packet types, so it alone does not identify the wire
codec. The corrected receiver maps authenticated advertised payload types independently
per source and carries the codec through reassembly, DAVE, prediction gating and playback.
Session announcements do not retire received media. Forward source codec changes retire
only affected buffered media; old primary/RTX packets and empty padding cannot switch it back.

A synthetic regression receives interleaved H.265/H.264 sources under an H.264 session
selection, preserving unrelated partial pictures and rejecting stale/unoffered formats.
The encrypted HEVC/RTX regression now traverses actual receive routing and DAVE decryption
into the timed playback queue, rather than bypassing protocol admission. Final native
H.265/H.264 timestamp-phase runs retain all 480/479 expected reference-identical pictures
and presentation tags with zero errors/resets/drops/rejections and no final audio concealment.
The rest of the native decoding/timing path is unchanged. The later owner-operated retest confirmed working reception and satisfactory playback
(see below); physical FPS/latency and full-client CPU/RSS remain unmeasured.

| Standard Arch package / same pinned toolchain | Before `169bfdc` | Routing correction | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,766,344 B | 92,767,368 B | +1,024 B (+0.001%) |
| Installed payload | 97,464,777 B | 97,465,801 B | +1,024 B (+0.001%) |
| Arch archive | 54,441,888 B | 54,449,636 B | +7,748 B (+0.014%) |

### UHD live reception — October 23, 2026

The verified preceding `ac345cc` package was preserved before editing. The original
1080p cap dates to the first RGBA receiver (`b0f57be`), without a recorded codec or
protocol requirement for that resolution. Live dimensions now accept 3840×2160 worth
of pixels within a 3840-pixel square. HEVC SPS coded surfaces allow bounded 64-pixel
alignment padding, with a separate visible-pixel check. Native frame copies share the
live validator; inline attachment playback retains its 1080p bound. No sender preset,
encoded access-unit limit, packet repair bound, timing rule or dependency changes.

The watched UI retains at most eight pending decoded pictures and 128 MiB including the
latest picture, allocated on demand. Eight packed UHD YUV pictures plus the latest one
fit; ordinary 720p/1080p use does not allocate the ceiling. Regression coverage exercises
UHD YUV presentation/item pressure, RGBA byte pressure, HEVC crop/padding rejection and
GStreamer sample copying before allocation. These are component bounds, excluding
native decoder reference pictures, GPU textures and other application allocations.

Eight-second synthetic `testsrc2` workloads pass through production clock, stream audio,
playout and native GStreamer decoding on Linux. Each includes a temporary +200 ms RTP
timestamp phase and simulated 73 ms audio output delay:

| Native functional workload | Expected/delivered pictures | Decoder errors / stale / queue resets / drops |
| --- | ---: | ---: |
| H.265 2560×1440/60 | 480 / 480 | 0 / 0 / 0 / 0 |
| H.265 3840×2160/30 | 240 / 240 | 0 / 0 / 0 / 0 |
| H.265 3840×2160/60, 12 Mbps target | 480 / 480 | 0 / 0 / 0 / 0 |
| H.264 3840×2160/60, 12 Mbps target | 479 / 479 | 0 / 0 / 0 / 0 |

H.264 retains its last parser picture without EOS. Every delivered luma checksum and
presentation tag matches its reference. Every case preserves all 100 audible frames
in the last two input seconds without concealment or admission rejection. The unchanged
receiver rejects the same 1440p/4K HEVC inputs before native decoding (zero pictures).
These single functional runs do not measure physical FPS, scanout, lip sync or full-client
CPU/RSS, and do not guarantee UHD60 throughput on other hardware.

The test harness uses optimized zlib checksums outside the application dependency graph.
Its previous debug Rust checksum helper alone cost about 14.4 ms per UHD picture and
caused false overload in 60 fps tests; those instrumented runs are not evidence of a
product decoder limit. Bitstreams use libx264/libx265 ultrafast, no B frames, AUDs and
repeated parameter sets; HEVC uses four pool/two frame threads. The UHD60 fixtures have
a 12 Mbps target/max rate and 12 Mbps VBV buffer.

An owner-controlled sequential comparison of `ac345cc` and official Discord on the same
stream recorded H.265, 1280×718 and approximately 30 fps in both clients, averaging
1.13/1.11 Mbps respectively. Neither capture recorded video drops; Serein recorded
zero decoding errors. The owner reported satisfaction. Content changes between captures
and bitrate accounting differences preclude an exact perceptual-quality claim.
Reception supports up to 4K60, but live 4K60 validation remains pending.
Windows/macOS native behavior is not locally verified. Discord's [official screen-share documentation](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share)
lists desktop streaming up to 4K/60 fps with Nitro.

| Standard Arch package | Before `ac345cc` | UHD candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,767,368 B | 92,767,752 B | +384 B (+0.000%) |
| Installed payload | 97,465,801 B | 97,466,185 B | +384 B (+0.000%) |
| Arch archive | 54,449,636 B | 54,440,009 B | -9,627 B (-0.018%) |

### Adaptive packet-repair allowance — October 23, 2026

An owner-operated test of `ffd6063` recorded 277 incomplete-frame events, 2,953 gated
predictions and 1,047 RTX packets, but only 29 holes repaired in 246.895 seconds.
Decoder errors, stale frames, decoder queue-full events and playout-pressure drops were
zero. The owner reported the same stream played smoothly in official Discord. These
counters expose repeated recovery but do not establish whether RTX was late or duplicated:
the preceding build did not measure first-NACK-to-hole-fill time. Its UHD dimension
diagnostic also truncated widths above 2047; the corrected packing retains 12 bits per axis.

The fixed 120 ms repair cutoff could discard every response on a slower path without
learning from it. Requested hole fills now increase the source's repair allowance to the
observed hole age plus 40 ms, capped at 400 ms. Intact ordered packets still have no
intentional holdback. At most 512 fixed-size expired-hole metadata records per source
survive for one second, without retaining media. Retired/changed sources discard that
history; late fills never resurrect discarded pictures. Packet/byte caps and three NACK
attempts remain unchanged. Numeric expired/late/response/budget aggregates let the next
owner test confirm or reject delayed repair as the explanation for this live failure.

A deterministic, no-sleep ordering workload compares preserved `ffd6063` with this
change: 600 one-packet pictures at 60 Hz, a missing non-key packet every 30 pictures,
usable keyframes every 60 pictures, and delivery of the missing packet at the configured
delay after its first NACK. The harness models reference gating from ordered loss events;
it does not run DAVE, native decoding, rendering or audio and is not a live FPS benchmark.

| First-NACK response delay | Prior / adaptive delivered pictures | Prior / adaptive loss events | Prior / adaptive gated predictions |
| --- | ---: | ---: | ---: |
| 40 ms | 600 / 600 | 0 / 0 | 0 / 0 |
| 180 ms | 150 / 555 | 20 / 1 | 430 / 44 |
| 450 ms (beyond cap) | 150 / 150 | 20 / 20 | 430 / 430 |

The first late loss still needs normal keyframe recovery before later holes benefit.
Unrecoverable holes may now wait longer, within the 400 ms limit. Regressions cover
sequence wrap, late duplicates, learning only from requested holes, metadata lifetime/count
and hard expiry. Full-client quality and physical latency still need an owner-operated retest.

An eight-second HEVC 3840×2160/60, 12 Mbps native functional comparison adds packet
fragmentation/reassembly to the earlier clock/audio/playout/GStreamer harness. One middle
packet is withheld from each non-key picture numbered 30 modulo 60, then restored 180 ms
after its first NACK. This bypasses transport/DAVE encryption (covered separately by the
protocol regressions) and simulates 73 ms of audio-device delay. Before/adaptive results:
8/1 incomplete-frame events, 0/7 repaired packets, 232/29 gated predictions and 240/450
delivered pictures out of 480 inputs. Every delivered picture matches its reference luma
and presentation tag. Both runs have zero decoder errors, stale/admission rejections,
playout resets/drops and final audio concealment, with all 100 audible frames in the last
two input seconds. The initial loss still produces a roughly half-second callback gap;
the adaptive run avoids repeating that loss on subsequent repairs. Callback timing is
not physical display timing. A separate approximately 44 Mbps 1440p fixture exceeds the
unchanged repair queue bound before delayed repair; both revisions retain that hard limit.

| Standard Arch package | Before `ffd6063` | Adaptive repair | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,767,752 B | 92,775,816 B | +8,064 B (+0.009%) |
| Installed payload | 97,466,185 B | 97,474,249 B | +8,064 B (+0.008%) |
| Arch archive | 54,440,009 B | 54,463,210 B | +23,201 B (+0.043%) |

### Receive-only AV1 — October 23, 2026

The verified `8f79435b` package is the baseline. Its owner-operated 120.212-second
sample receives HEVC at 2560×1440, 59.49 decoded fps and 1.234 Mbps of complete decrypted
video payload, with zero incomplete/gated frames, decoder errors or playback drops.
The same owner then opens the stream in official Discord: 13 post-startup reports span
120.389 seconds, report `AV1X` at 2560×1440, 59.99 decoded fps and mean 4.594 Mbps
(range 4.327–4.979 Mbps), with zero reported drops. Sequential motion/content and
accounting differ; the ratio is not a codec-efficiency benchmark or proof of why the
service selected those rates. The preceding Serein build cannot negotiate/depacketize AV1.

Linux receive-only AV1 now takes priority over HEVC/H.264 after installed native capability
probing. No dependency or sender-preset change is introduced. The pipeline uses existing
GStreamer AV1 parsing and ranked decoding, the same planar output and presentation tags.
After DAVE authentication, Main-profile sequence/surface bounds are enforced before native
submission; explicit plaintext OBU sizes are restored for the native parser. RTP has a
64-element packet / 256-OBU frame ceiling. Reassembly's frame and fragment buffers are
each bounded to 2 MiB + 64 KiB, with large fragment capacity released at frame reset;
normalization temporarily holds one extra bounded AU on the decoder worker. Existing
encoded playout/decoder/display caps, adaptive repair and audio clocks are retained.

An eight-second synthetic 2560×1440/60 SVT-AV1 low-delay CBR fixture targets 4.5 Mbps,
with four logical encoding processors, preset 12 (mapped by SVT 4.2.0 to preset 11),
key interval 60, 8-bit 4:2:0. Its native Linux GStreamer workload includes a temporary
+200 ms RTP timestamp phase and simulated 73 ms audio-device delay. All 480 pictures
match reference luma and presentation tags, with zero decoder errors, stale/admission
rejections, playout resets/drops and final concealment. All 100 audible frames in the
final two input seconds survive. This native workload bypasses RTP/DAVE; the extended
encrypted protocol regression covers aggregation/fragmentation/RTX through DAVE and
playout. Bounds tests cover malformed OBU lengths/counts, forbidden/spatial headers,
fragment continuity and truncated/oversized/unsupported sequence headers.

These are functional checks, not physical FPS, latency or perceptual-quality measurements.
Instrumented AV1 UHD/native Windows/macOS behavior and full-client CPU/RSS are unmeasured.
The subsequent successful Linux 1440p60 capture is recorded below; live 4K60 testing remains pending.

| Standard Arch package | Before `8f79435b` | AV1 reception | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,775,816 B | 92,785,288 B | +9,472 B (+0.010%) |
| Installed payload | 97,474,249 B | 97,483,721 B | +9,472 B (+0.010%) |
| Arch archive | 54,463,210 B | 54,465,029 B | +1,819 B (+0.003%) |

### Watched-stream transport feedback — October 23, 2026

The verified `fcf6fe62` package is the baseline. The owner-controlled different-stream
HEVC 1280×534 comparison records 1.299 Mbps of Serein complete decrypted payload over
100.133 seconds, versus official Discord's mean reported 3.841 Mbps across eight
post-startup reports spanning 70.234 seconds. Both decode about 59.8 fps without video
drops. Serein records four audio rebufferings and scheduling jitter. Different scenes
and accounting prevent treating the ~2.96× ratio as causal bandwidth evidence.

The receiver adds transport-wide arrival acknowledgements, not a receiver bandwidth
estimator. History is a single on-demand 2,048-entry deque of optional 64-bit timestamps
(maximum 32 KiB), with one-second reporting age and bounded restart/overflow retirement.
Per report, scratch holds at most 256 one-byte symbols, 512 delta bytes and a 608-byte
output allocation. Actual encrypted datagrams are at most 628 bytes, at most two per
100 ms: a hard 12,560-byte/s datagram ceiling excluding IP/UDP headers. Ordinary reports
are smaller; no feedback is emitted without an authenticated transport sequence.
Only the watched-stream transport uses this path; decoding, presentation queues, sender
presets and dependencies are unchanged. Synthetic wire tests cover complete known arrival
patterns, signed reordering, missing statuses, wrap, duplicate rejection and all bounds.
These verify protocol behavior, not SFU acceptance, bitrate uplift or full-client CPU/RSS.
New numeric transport/probe byte counters allow the next live comparison to distinguish
wire traffic from complete decrypted media. Owner testing remains required.

| Standard Arch package | Before `fcf6fe62` | Transport feedback | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,785,288 B | 92,801,160 B | +15,872 B (+0.017%) |
| Installed payload | 97,483,721 B | 97,499,593 B | +15,872 B (+0.016%) |
| Arch archive | 54,465,029 B | 54,472,967 B | +7,938 B (+0.015%) |

### AV1 temporal delimiter repair — October 24, 2026

The verified `bc9b2682` package is the baseline. The owner's AV1 2560×1440 attempt
receives complete video payload up to roughly 7.5 Mbps, with transport-wide arrival
feedback active, but displays only one picture per attempt. Incoming audio continues
without concealment. This is evidence of received packets, not successful AV1 playback.

The earlier eight-second 1440p60 SVT-AV1 IVF workload included 480 temporal delimiter
OBUs that RTP normally omits. Removing those delimiters while preserving every other
OBU/pixel and IVF timestamp reproduces one native picture from 480 inputs. Restoring
one empty leading delimiter during post-DAVE native normalization recovers all 480
reference-luma-identical pictures and matching presentation tags, with zero decoder
errors, stale/admission/playout drops or final concealment. The workload retains the
+200 ms video timestamp phase and simulated 73 ms audio-device delay, and all 100
audible frames in the final two input seconds. It bypasses RTP/DAVE; existing encrypted
wire tests separately cover their framing. The regression extends the OBU framing test
to require native TU delimiters and preserve existing leading delimiters.

Native normalization adds only two bytes when needed, within the existing AU bound.
Parameter/keyframe admission inspects authenticated wire OBUs before native framing is
added. No dependency, codec negotiation, queue limit or feedback change is introduced.
These are synthetic functional measurements; the subsequent owner playback evidence
is recorded below. Full-client CPU/RSS remains unmeasured. The prior delimiter-retaining fixture did not exercise
this boundary and must not be treated as evidence that RTP-shaped AV1 had worked.
The final native replay runs after Cargo checks; a retained concurrent run hit startup
queue-age expiry and is excluded from this functional before/after comparison.

| Standard Arch package | Before `bc9b2682` | AV1 temporal framing | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,801,160 B | 92,801,416 B | +256 B (+0.000%) |
| Installed payload | 97,499,593 B | 97,499,849 B | +256 B (+0.000%) |
| Arch archive | 54,472,967 B | 54,475,582 B | +2,615 B (+0.005%) |

### Final owner playback evidence — October 24, 2026

Runtime candidate `e935dc7b` receives AV1 2560×1440. The reviewed post-startup
696.615-second interval delivers 41,800 pictures at 60.004 fps, with mean complete
video payload 7.892 Mbps (five-second range 3.722–8.385 Mbps) and authenticated video
RTP datagrams 8.249 Mbps. Decoder errors, stale frames, queue/playout drops/resets,
incomplete/DAVE-decrypt failures, audio concealment and rebufferings are all zero.
Six packets are repaired without expiry; 6,231 transport feedback reports are sent,
with zero missing-status or discarded-history counts in the interval. An earlier
80.133-second steady sample is also clean; startup records one audio rebuffer.

Smooth playback is confirmed for this Linux 1440p60 test. Reception supports up to
4K60, but live 4K60 validation remains pending. These results do not prove a causal
bitrate improvement, identical content/perceptual quality versus official Discord, physical 60 Hz presentation,
scanout/latency or full-client CPU/RSS. The numeric evidence is kept private and only
aggregate results are recorded in the repository.

### Review validation — October 25, 2026

Four focused regressions cover call speech queued across stream retirement, rejection of
retired PCM in the stream bridge, standalone H.264 parameter updates between pictures,
and delayed native output across sparse inputs and tag pressure. Timing correlation keeps
at most 16 entries; unknown/evicted native tags are dropped with a stale counter rather
than admitted as untimed pictures. Parameter-only software inputs no longer enter the
picture timestamp FIFO. Already mixed call speech survives stream timestamp invalidation;
stream-only stale audio still drops, and call playback/security gates remain effective.

The full workspace gate passes 1,321 tests plus strict Clippy, formatting, production and
policy checks. The offline playback example also passes. Serial before/after runs of the
same eight-second RTP-shaped AV1 1440p60 fixture (baseline `e935dc7b`, review candidate)
retain all 480 identical luma checksums and presentation tags, with no decoder errors,
stale/queue/playout drops or resets, and all 100 audible frames in the final two input
seconds without concealment. These functional runs include a temporary +200 ms video
phase and simulated 73 ms output delay. They do not measure physical display/audio latency
or establish a speed improvement. The existing owner live results above predate these
review changes; no further live validation is claimed here.

| Standard Arch package | Before review (`e935dc7b`) | Review candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,801,416 B | 92,802,184 B | +768 B (+0.001%) |
| Installed payload | 97,499,849 B | 97,500,617 B | +768 B (+0.001%) |
| Arch archive | 54,475,582 B | 54,477,570 B | +1,988 B (+0.004%) |

### Upstream integration — October 27, 2026

The initial integration (`7f0f326d`) incorporates upstream `d93a8415`, retaining its
media-thread teardown and output-format selection alongside stream timing. Formatting
and strict Clippy pass.
The workspace unit run passes 1,338 tests and fails the unchanged
`categories::tests::collapsed_categories_follow_service_order_selection_and_buttons` at
`crates/ui/src/categories.rs:2240`; an isolated rerun fails identically. The same assertion
fails in [upstream Linux CI](https://github.com/ViceVerse-cz/Serein/actions/runs/40329311638/job/121594172966).
Production, policy and doctest checks pass. The full `cargo xtask check` is therefore not claimed
as passing for this integrated revision. No sidebar test or implementation is changed here.
Upstream also has an unchanged [fuzz fixture compilation failure](https://github.com/ViceVerse-cz/Serein/actions/runs/40329311638/job/121594172868)
(`Role::managed` is missing from its initializer) and the previously reported dependency
security audit failure. None is suppressed by this integration.

The native Arch package smoke passes. Standard voice-enabled Rust 1.98.1 packages:

| Package metric | Before integration (`5c89c95c`) | Integrated candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,802,184 B | 92,995,848 B | +193,664 B (+0.209%) |
| Installed payload | 97,500,617 B | 97,694,281 B | +193,664 B (+0.199%) |
| Arch archive | 54,477,570 B | 54,559,045 B | +81,475 B (+0.150%) |

The integrated package contains 245 installed files. These deltas include all
upstream changes through `d93a8415` and do not isolate the stream feature. No new physical
playback or CPU/RSS claim is made.

### CI follow-up — October 27, 2026

The sidebar keyboard fixture now gives its post joined/pinned state before navigating
to the forum, so the post remains listed under the current sidebar rules. The fuzz
role fixture includes `managed`. The recovery socket test waits for a heartbeat
carrying a new dispatch sequence after acknowledging its healthy probe, before
requesting the unanswered probe. It deliberately queues and drains an older
heartbeat first, and sends no late acknowledgement that could cancel the next
probe. It no longer depends on repeated 200 ms polling. These changes
affect test fixtures only, with no sidebar or gateway runtime behavior change.

`cargo xtask check` passes: 1,349 unit/integration/doctests pass, 14 are ignored,
and formatting, strict Clippy, production and policy checks pass. Both pinned-nightly
fuzz smoke targets pass (193,110 decode inputs and 6,596 state-transition inputs,
31 seconds each). Declared-license checks for both lockfiles and the six offline
license-policy fixtures pass. Hosted Windows/macOS validation remains pending for
this follow-up; Linux test success does not establish a macOS runtime result. The
recovery probe test also passes 20 consecutive Linux runs with the patched Tokio.

Compatible dependency patches:

| Package | Initial integration | Follow-up |
| --- | --- | --- |
| ctutils | 0.4.2 | 0.4.6 |
| rustls-webpki | 0.103.15 | 0.103.16 |
| webpki-root-certs / webpki-roots | 1.0.9 | 1.0.10 |
| Tokio | 1.53.1 | 1.53.3 |
| clipboard-win | 5.4.1 (yanked) | 5.4.3 |

With cargo-audit 0.22.2 and the same RustSec database revision
`3279c99911354c400e1d41e47c7d60e39258b147`, the strict audit changes from five
vulnerabilities/two denied warnings to one vulnerability/no denied warnings. It
still fails on [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html)
in crypto-bigint 0.5.5, used by DAVE's P-256/OpenMLS stack. Released Davey 0.1.5
and OpenMLS providers still require the affected P-256 dependency line. Resolving
that finding needs a coordinated crypto migration; this follow-up does not suppress
it or change the DAVE ciphersuite. No new dependency, license exception or media
buffer is added. Playback limits and prior live-test limitations remain unchanged.

The standard voice-enabled Rust 1.98.1 Arch package passes its native metadata,
dependency, payload, ownership and mode smoke checks:

| Package metric | Initial integration (`7f0f326d`) | Follow-up | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,995,848 B | 92,995,592 B | -256 B (-0.0003%) |
| Installed payload (245 files) | 97,694,281 B | 97,694,025 B | -256 B (-0.0003%) |
| Arch archive | 54,559,045 B | 54,561,319 B | +2,274 B (+0.0042%) |

One artifact set per revision, unchanged release settings and package contents;
archive timestamps/compression add noise. These small size differences are not
evidence of faster playback. CPU/RSS and physical A/V latency are unmeasured.

### Upstream integration — November 1, 2026

The stream branch incorporates upstream `54512d29`, including audio-device recovery,
camera recovery, custom keybinds and opt-in local diagnostics. The performance-document
conflict retains the stream-playback evidence and upstream's push-to-talk measurements; the
fuzz fixture retains the same assignable role and upstream's explanatory comment.

All 1,398 workspace unit/integration/doctests pass, with 14 ignored. Formatting,
auth-handoff fixtures, declared-license checks and the six license-policy fixtures pass.
The full `cargo xtask check` does not pass: unchanged upstream
`crates/platform/src/hotkeys.rs` fails strict Clippy on its Linux-unused `AtomicBool`
import, `MOUSE_ID` constant and eight-argument `portal` function. No hotkey implementation
or lint suppression is changed by this integration.

Upstream now ignores RUSTSEC-2026-0387 and several other advisories and disables yanked
package checking. This branch inherits that policy unchanged; its compatible dependency
patches remain. Ignoring the crypto-bigint advisory does not remediate it. Earlier strict
audit results above describe the earlier policy, not this integrated revision. No new
owner-operated live stream test or physical latency/CPU/RSS measurement is claimed.


The standard voice-enabled Rust 1.98.1 Arch package passes native metadata, dependency,
payload, ownership and mode checks. Production and policy checks also pass separately.

| Package metric | Previous runtime (`406fbb9c`) | Integrated candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 92,995,592 B | 93,430,600 B | +435,008 B (+0.468%) |
| Installed payload | 97,694,025 B | 98,129,053 B | +435,028 B (+0.445%) |
| Arch archive | 54,561,319 B | 54,751,460 B | +190,141 B (+0.348%) |

The integrated payload contains 245 files. One artifact set per revision, unchanged
release settings; archive timestamps/compression add noise. These deltas include upstream
changes through `54512d29` and do not isolate stream-feature cost.

### Upstream integration and parser security patch — November 3, 2026

This integration incorporates upstream `23010ac7`, retaining its stereo Opus microphone
encoder alongside the stream timing and receive logic. The encrypted loopback regression
verifies stereo 1,275-byte outgoing packets and mono decoding of a full 20 ms frame.

The direct pulldown-cmark pin advances from 0.13.5 to 0.13.6 for RUSTSEC-2026-0433.
Serein does not enable the affected wikilinks option; the patched MIT-licensed release
is used without adding an audit exception. With cargo-audit 0.22.2 and the same database
`263a4d951a0867d4bddc29dddd6d59427c78f9a7`, the old lockfile reports that one finding
and the patched lockfile reports none, with no denied warnings. Both runs use the same
inherited audit policy; ignored upstream advisories are not claimed as remediated.

`cargo xtask check` passes: 1,443 unit/integration/doctests, 14 ignored, formatting,
strict Clippy, production and policy checks. Declared-license and six license-policy
fixtures pass. Native Arch metadata, dependency, payload, mode and ownership smoke
checks pass. No fresh owner-operated live or physical A/V quality result is claimed.

| Package metric | Before parser patch (`a6c6e80a`) | Patched candidate | Delta |
| --- | ---: | ---: | ---: |
| Executable | 93,495,880 B | 93,496,008 B | +128 B (+0.000%) |
| Installed payload | 98,194,333 B | 98,194,461 B | +128 B (+0.000%) |
| Arch archive | 54,776,341 B | 54,782,011 B | +5,670 B (+0.010%) |

Installed payload: 245 files. One artifact set per revision, same standard
voice-enabled release settings; archive compression/timestamps add noise. CPU/RSS and
physical latency are unmeasured; no speed improvement is inferred from package size.

### Earlier playback recovery evidence

The prior owner-tested candidate `fb093bc` covered two sessions totaling 6,638.153 seconds
at 1280×720/~60 fps: 397,845 decoded pictures, zero decoder errors/stale submissions, five
reset events and 48 queued-picture drops. One reset is in the final teardown window.
There were four incomplete pictures, a longest decoded-picture interval of 2,359 ms,
and no five-second window with >100 decrypted audio packets but zero produced audio frames.
The owner reported smooth playback with no major hiccups. Numeric counters cannot establish
physical lip sync or identify the cause of every isolated pause. This capture predates the
adaptive/device-clock changes; the new candidate still needs the owner's comparison.

The design review reproduced a startup failure: an eight-packet window repeatedly restarts
before a 100 ms deadline when valid Opus packets are 2.5/5/10 ms long. The corrected 128-packet
budget covers 320 ms at the minimum packet duration, including the 240 ms maximum adaptive
target and scheduling headroom. A synthetic regression sends warmed, valid Opus continuously
for three minutes at each 2.5/5/10/20/40/60 ms duration with sender drift of −500/0/+500 ppm.
All 18 cases produce all 50 expected audible output frames in the final second. Time is
synthetic; no sleeps, audio devices or live content are used. Tests also cover initial buffering,
reordering, timestamp wrap/jumps, bounds, source retirement and PCM clearing.

The `0fcf859` owner comparison exposed crackling and recovery pauses. Its 249.930-second
numeric capture contained 11 playout resets and 172 queued-picture drops, with a longest
picture interval of 1,439 ms. No device/parent transport drops or 80 ms stalls were reported;
video queue depth grew to 43. These counters do not identify every source of the crackling.

Two additional offline regressions reproduce a 40 ms scheduling burst interrupting the
stream/call PCM bridge, and audio starvation invalidating the shared video/queued-PCM
generation. The bridge now buffers 40 ms of scheduling headroom with an eight-frame cap;
audio rebuffering resets its RTP anchor without invalidating accepted media. Source/security
changes still invalidate it. A 60-tick synthetic burst workload had six sample-order gaps
before and zero after (53 versus 58 output frames, with the new initial two-tick hold).
Five 300-second synthetic tone cases (ordered/bursty arrivals, packet loss with 1,000 ppm
drift, and variable poll timing) show no >0.05 adjacent-sample jumps after warmup on either
revision; this does not reproduce or rule out physical-device crackling. The new counters
separate decoded/concealed samples, rebufferings, queue span/target, bridge discontinuities
and the OS device-delay estimate. A further owner comparison remains required.

The next `44a1fbc` owner capture's initial 34.380-second segment has 10 video queue
resets / 239 queued-picture drops and a 1,039 ms picture interval, while audio has zero
concealment or rebufferings; device empty-frame counters stay zero in intervening full
playback windows. Its longer ongoing segment later
generated 4,546,560 concealment samples at the frozen evidence cutoff, despite continuing
valid input. These counters distinguish separate video-timing and audio-cursor failures;
the old capture did not record the individual reason for every video queue reset.

A temporary +200 ms video timestamp phase followed by a return to the original epoch
reproduces reference-chain loss. Local scheduling now absorbs that phase step at arrival
cadence and never rewinds behind queued pictures; authenticated sender-clock correction
still slews phase gradually. A one-second sparse source gap retains its ordinary RTP timing.
An independent −200 ms audio epoch reproduced decoding valid packets only to discard their
PCM and generate concealment. The per-source sequence/timestamp watermark now distinguishes
an actual backwards sample epoch from packet reordering and restarts buffering promptly.
The strengthened regression verifies decoded input is not replaced by concealment; the old
non-silence assertion passed even while generated replacement audio was playing.

The delay envelope has bounded attack/release and a 60–240 ms target. A filtered occupancy
controller interpolates at most five extra/fewer input samples per 960-sample output frame.
This is bounded resampling, not pitch-preserving time stretching. Video synchronization uses
elapsed time, smoothing and a deadband, with a maximum 60 ms/s correction at both 30 and 60 fps.
A regression reaches a matching logical audio deadline within 10 ms at both rates. Output callback
regressions verify that the corresponding stream timestamp survives 44.1 kHz device resampling,
and stale generations cannot redirect the new clock.
An hour-long synthetic timeline at ±500 ppm keeps video within 10 ms of matching audio
output by anchoring playable audio positions to the actual producer clock, avoiding
accumulation against a nominal sample clock. The Opus matrix also includes repeated
60 ms delivery bursts followed by stability, verifying target growth and slow reduction. UI tests cover future-picture retention,
late decoded-picture skipping and the combined allocation limit.

One native functional run per case uses the same eight-second 1920×1080/60 FFmpeg `testsrc2`
fixture, libx264 veryfast/zerolatency, four threads, 8 Mbps target/max/VBV, AUDs, key interval 120,
no B frames or scene cuts. The production clock/audio/playback/native decoder runs through
GStreamer/VA-API on an AMD RX 6900 XT, with a simulated 73 ms audio-device delay. Each decoded
luma CRC and its correlated presentation tag must match its input in order. The parser retains
the final access unit; 479 outputs from 480 inputs are expected without EOS.

| Earlier native case / metric | `0fcf859` | Rebuffering correction `44a1fbc` |
| --- | ---: | ---: |
| 500 ms audio gap: decoded pictures | 479 | 479 |
| Same case: invalidated presentation tags | 10 | 0 |
| Same case: maximum decoded-picture interval | 110.933 ms | 25.875 ms |
| Same case: playback resets / drops | 0 / 0 | 0 / 0 |
| Audio arrives 400 ms after video: decoded pictures | 479 | 479 |
| Audio RTP advances 200 ms: audible frames in final two seconds | 100 | 100 |

That gap comparison is one run per revision from the preceding rebuffering correction. The delay/jump baselines come from the
preceding candidate's recorded runs using the same fixture and method. All corrected cases
have 479 reference-identical pictures with correlated, current presentation tags, zero decoder
errors, stale submissions, admission rejection or playback queue resets/drops. Encoded queue
peaks for gap/delay/jump are 9/32/20 pictures. Delay/jump maximum decoded-picture intervals
are 127.533/20.862 ms. Acquiring audio during video still causes a brief timing adjustment.
These single functional runs are not isolated benchmarks or GUI/physical measurements.
The new-source clock wait remains capped at 200 ms and expires when output timing is unavailable.

The latest native comparison uses `44a1fbc` as its baseline, the same fixture/backend,
and one run per case. Timestamps change while encoded picture order remains intact.

| Timestamp continuity metric | `44a1fbc` | Corrected candidate |
| --- | ---: | ---: |
| Video phase change: decoded pictures | 358 | 479 |
| Same case: gated predictions | 99 | 0 |
| Same case: queue resets / drops | 1 / 20 | 0 / 0 |
| Same case: maximum decoded-picture interval | 2,026.628 ms | 20.687 ms |
| Backwards audio epoch: generated concealment samples in final two input seconds | 95,040 | 0 |
| Same audio case: non-silent output frames | 100 | 100 |

Every corrected case (phase/backwards-audio/gap/delayed-audio/forward-audio epoch) has all
479 reference-identical pictures and correctly correlated, current presentation tags; zero
decoder errors, stale submissions, queue rejection or playback resets/drops; all 100 expected
audible frames in the final two input seconds with zero concealment then. Delay/jump picture
interval maxima are 107.366/18.751 ms in this run; the initial audio-acquisition adjustment
remains. These are functional measurements, not physical playback guarantees. Independent
continuous-tone input at ±200 ms audio epoch shifts produces 48,000 real decoded samples
and zero concealment in the final second; the old negative-epoch path decoded 48,000 but
also generated 48,000 replacement samples. Four 100-second synthetic clock patterns now
have zero reference queue resets/drops; the phase case previously had 19 resets / 190 drops.
New fixed counters distinguish clock rewinds, source switches, expiry, pressure and ahead
limits in the next owner comparison. No physical smoothness/listening confirmation is claimed.

Earlier component comparisons against the original receiver isolate why packet repair/pacing
is included. Six-picture bursts delivered 18 → 479 pictures; an empty keepalive caused
89 → 0 gated predictions and 390 → 479 delivered pictures. A simulated 120 Hz latest-picture
consumer's upload-gap p95 changed from 91.961 → 34.011 ms at 30 fps and 75.005 → 17.010 ms
at 60 fps. These are preparation evidence on earlier integration builds; they do not measure
the current presentation queue, default-client CPU/RSS or physical display timing. The old A/V
candidate `0dabf19` also exposed a 16-picture overflow and a retained audio-anchor starvation
loop; their synthetic regressions remain covered.

Resource limits: repair 512 sequence positions / 512 KiB per source (16 sources), encoded
presentation queue 64 pictures / 8 MiB, decoder handoff 16 pictures / 16 MiB, Linux native input
four units, native decode-age budget 150 ms. Watched audio retains at most 128 compressed
packets and 5,760 mono PCM samples per source (16 sources; 2,979,840 payload/PCM bytes), plus a
965-sample stack scratch. Correlation keeps at most 16 tags per decoder until output or
replacement. UI pending/latest planes share an eight-pending-picture / 128-MiB
allocated-capacity budget. Existing bounded
PCM channels remain; the stream/call bridge pending cap increases from two to eight frames
(7,680 to 30,720 sample bytes, excluding metadata), with 40 ms startup headroom. Timestamps
add metadata, not another queue. Diagnostics stay opt-in and retain only bounded numeric aggregates.

The model follows [RTP sender-clock mapping](https://www.rfc-editor.org/rfc/rfc3550.html#section-5.1),
[WebRTC's adaptive audio-buffer approach](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/g3doc/index.md)
and [separate decode/render scheduling](https://webrtc.googlesource.com/src/+/refs/heads/main/video/frame_decode_timing.cc),
with smaller local policies. It does not implement all NetEq behavior or claim generic AVPF/CNAME
conformance. CPAL output timestamps and egui repaint deadlines estimate presentation; actual
headset/monitor latency and scanout are unmeasured. Whole-client CPU/RSS and Windows/macOS native
timing behavior are unmeasured; no improvement is claimed for those metrics. Screenshots are not
applicable because no UI layout changes. No live media was recorded.

## Shared output-format selection — October 24, 2026

Fresh baseline `38e46b8f5ec694002ef515fbc2d8dc69297e4f5f` and changed source were built
with Rust 1.98.1 and locked dependencies on Arch Linux x86_64 (Ryzen 9 5900XT,
20 visible logical CPUs, 61,634,584 KiB total RAM). Standard release packages include
voice and disable default/development-data and demo features. The baseline package and
an initial selector sample were captured before edits; the table uses final probes run
serially after checks, with no concurrent Rust build. Package outputs were kept separate.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 92,650,248 B | 92,648,840 B | -1,408 B (-0.0015%) |
| Installed native payload | 97,348,681 B | 97,347,273 B | -1,408 B (-0.0014%) |
| Arch distribution archive | 54,365,367 B | 54,366,835 B | +1,468 B (+0.0027%) |
| Notification, usable default, median ns/selection | 0.984 | 1.645 | +0.660 ns |
| Notification, I24 default / 64 ranges, median ns/selection | 0.837 (rejects) | 86.922 (selects) | +86.085 ns; recovery added |
| Voice, 64 ranges, median ns/selection | 75.227 | 89.032 | +13.805 ns (+18.35%) |

Each installed payload contains 245 files. Default
`cargo xtask package` builds the release executable, then fails because `dpkg` is absent.
`cargo xtask package --format arch` passes native package inspection at both revisions;
the existing `libz.so` dependency warning occurs in both. Sizes sum regular installed files
under `usr/` and measure the entire `.pkg.tar.zst` archive, including metadata/timestamps.

The offscreen probe uses the actual selector source with synthetic device queries, compiled
with `rustc -O -C lto=thin` and CPAL from the locked release build. Each workload has one
warmup plus five runs of 1,000,000 selections. The I24 fixture changes from 0 / 1,000,000
successful selections to 1,000,000 / 1,000,000; the extra scan produces a usable stereo F32
48 kHz selection. Voice's small absolute setup-cost increase is recorded above. The trivial
default paths approach timer/optimizer limits; no runtime performance improvement is claimed.

Selection visits at most 64 advertised ranges and accepts only F32/I16/I32/U16, 1–8 channels
and 8–192 kHz. Native OS queries, stream startup, audio callbacks, audible latency, CPU/RSS
and UI timings are unmeasured. Windows policy is simulated on Linux; native WASAPI/macOS
and physical I24 playback remain unvalidated. The license checker is unavailable because
`cargo-deny 0.20.2` is missing; no dependency versions or notices change. No historical
platform evidence is reused, and no live Discord, calls, microphone or credentials are used.
[Method and reproduction](pr-evidence/shared-output-format-selection/README.md),
[raw samples, package hashes and source identities](pr-evidence/shared-output-format-selection/measurements.json).

## Server folder mark-as-read — October 15, 2026 (PR #672)

Compared base `104b1aa5` with folder mark-as-read `2b02b49e`. Standard unsigned Windows x64
portable packages use Rust 1.98.1 and `cargo xtask package` (voice included, demo disabled).
Each installed tree contains 216 files; ZIP output uses .NET `Compress-Archive`. NSIS was
unavailable, so no installer was measured.

| Package metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| `serein.exe` | 87,399,936 B | 87,405,568 B | +5,632 B (+0.0064%) |
| Installed tree | 91,593,225 B | 91,598,857 B | +5,632 B (+0.00615%) |
| ZIP archive | 51,350,178 B | 51,354,785 B | +4,607 B (+0.00897%) |

Release demo builds at both revisions ran the same offline `--demo` workspace with the
synthetic folder context menu open in a 1120×760 window. The after menu shows the new row disabled
because this fixture has no unread guild in that folder. On Windows 11 Pro 10.0.26300, Ryzen 7
7800X3D (8 cores / 16 logical), 31.4 GiB visible RAM and AMD Radeon RX 7900 GRE (DX12),
process counters were sampled once per revision after a five-second warmup, 21 samples about one
second apart. CPU is the `Get-Process` CPU-time delta divided by actual elapsed wall time, where
100% is one logical core. Memory reports peak and mean of the final five samples.

| Native demo metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| CPU, one-core percentage | 1.4795% | 0.3893% | -1.0902 percentage points |
| Working set, peak / final-five mean | 185,057,280 B | 167,518,208 B | -17,539,072 B |
| Private bytes, peak / final-five mean | 204,038,144 B | 185,970,688 B | -18,067,456 B |

These single samples are descriptive only; no runtime improvement is claimed. Startup and frame
latency were not measured. The synthetic screenshots, raw samples and reproduction details are in
[`docs/pr-evidence/folder-mark-as-read/`](pr-evidence/folder-mark-as-read/). Live Discord behavior
and clicking the action against a folder containing unread guilds remain outside this offline run.
A later revision of the PR replaced the per-guild queue with the home tile's bulk acknowledgement
scoped to the folder's servers; it was not re-measured.

## Name styles and bundled name fonts - October 15, 2026

Compared baseline `5f83077e` (`main`) with the name-styles branch on Linux (CachyOS, x86_64),
both built with `cargo xtask package` in separate worktrees and target directories. On this
host the `.deb` stage fails in `dpkg-shlibdeps` for both revisions, so the measurement uses
the release executable and the staged `dist` payload produced before that stage. Compressed
size is `zstd -19` of the executable alone.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release executable bytes | 88,266,920 | 91,822,792 | +3,555,872 (+4.03%) |
| Staged `dist` payload bytes (no archives) | 92,445,973 | 96,065,437 | +3,619,464 (+3.92%) |
| Executable, `zstd -19` bytes | 42,744,679 | 44,296,091 | +1,551,412 (+3.63%) |

The growth is the 14 embedded display-name fonts (3,493,132 bytes) and their license texts.
The faces are embedded but parsed only when a visible styled name first needs one; with name
styles off (the default) none is parsed. CPU, process memory and frame timings were not
sampled, and no runtime improvement is claimed.

## Game mention catalog and review fixes - October 14, 2026

Compared baseline `7759f5b5` with game mentions and review fixes at `75c4dfe0`.
Both unsigned Windows portable packages use Rust 1.98.1 and `cargo xtask package`
(`--release --locked --no-default-features`, voice included). The baseline `dist`
was preserved in its separate worktree. The after measurement uses a clean copy
of the generated package, excluding an obsolete `ruzstd-0.9.0-LICENSE` left in the
reused output directory; that original file was preserved. Both measured packages
contain 216 files. ZIP uses Python deflate level 9 with sorted relative paths.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable bytes | 87,321,088 | 87,412,736 | +91,648 (+0.105%) |
| Full portable package bytes | 91,514,377 | 91,605,995 | +91,618 (+0.100%) |
| ZIP distribution bytes | 50,312,736 | 50,338,891 | +26,155 (+0.052%) |

NSIS was unavailable, so installer size was not measured. Native UI testing was
left to the owner at their request; CPU, process memory and frame timings were
not sampled. These package sizes do not measure catalog latency or live Discord
behavior, and no runtime performance improvement is claimed.

## Hover preload review repairs - October 14, 2026

Compared the original PR #636 head `08a9ae01` with runtime repair `d4b27dd3`, which also
integrates `main` at `cf5452cd`. This comparison does not isolate the five fixes from main changes.
Both fresh unsigned Windows portable packages use Rust 1.98.1 and `cargo xtask package`
(`--release --locked --no-default-features`, voice included), on Windows 11 Home 10.0.26200,
Ryzen 7 7800X3D, 33,410,678,784 bytes visible RAM. Separate worktree `dist` outputs were
preserved. Installed bytes sum all 216 files; ZIP uses Python deflate level 9 with sorted paths.
The built release replay ran directly: one warmup and five measured runs of 100,000 events.

| Metric / method | Original PR head | Review repair | Delta |
| --- | ---: | ---: | ---: |
| Package executable | 86,960,640 B | 87,321,088 B | +360,448 B (+0.41%) |
| Installed tree (216 files) | 91,153,929 B | 91,514,377 B | +360,448 B (+0.40%) |
| ZIP, deflate level 9 | 50,180,156 B | 50,312,732 B | +132,576 B (+0.26%) |
| Reducer replay, median of 5 | 73.1705 ms | 68.7076 ms | -4.4629 ms (-6.1%) |
| Retained timeline estimate | 393,492..393,977 B | 393,492..393,977 B | 0 |
| Navigation revalidation requests, 10,000 reselections | 0 | 0 | 0 |
| Hover first visits opened without waiting / requests | 12/12 / 12 | 12/12 / 12 | 0 |
| 28-row hover sweep requests | 12 | 12 | 0 |

The replay run ranges overlap (before 69.0651..76.5154 ms, after 66.9713..71.3841 ms); no speedup is claimed. Navigation retains 2 dormant windows / 150 rows / 328,148 estimated bytes on both revisions; the hover pass retains 8 windows / 983,809 estimated bytes. Core selection median/p95 is 300/300 ns on both, near timer resolution. These are bounded allocation estimates, not RSS.

NSIS is unavailable, so no installer was produced. The existing OpenH264 linker debug-info
warning appears in both builds. Native interaction capture and CPU/RSS sampling were unavailable
(no native computer-use API, no `orca` executable). No live Discord or microphone test was used.
The original feature screenshots are synthetic framebuffer exports, not native interaction proof.
[Per-run outputs and executable hashes](pr-evidence/hover-preload/measurements-review.json)
and [reproduction scenarios](pr-evidence/hover-preload/README.md) retain the evidence boundaries.

## Hover preloading — October 13, 2026

Release builds of baseline `2df8256c` (main) and the hover-preload branch with default features
(voice included), Rust 1.98.1, Windows 11 (build 26200), Intel Core i9-13900K, 32 GiB RAM. The
built `replay-bench` ran directly: one warmup and five measured runs, medians shown. These are
synthetic reducer workloads; they count requests and core selection time, not network latency,
display latency or RSS.

| Metric / method | Baseline | Hover preload | Delta |
| --- | ---: | ---: | ---: |
| Package executable (`xtask package`) | 86,803,968 B | 86,869,504 B | +65,536 B (+0.08%) |
| Installed tree (`dist/`, 216 files) | 90,997,257 B | 91,062,793 B | +65,536 B (+0.07%) |
| Installed tree, zip deflate level 9 | 50,161,038 B | 50,183,621 B | +22,583 B (+0.05%) |
| `replay-bench`: 100,000 events | 58.1 ms | 58.5 ms | noise (+0.6%) |
| `replay-bench`: retained timeline | 393,492..393,977 B | 393,492..393,977 B | 0 |
| `--navigation`: revalidation requests, 10,000 reselections of three 50-message conversations | 10,000 | 0 | −10,000 |
| `--navigation`: select median / p95 | 200 / 300 ns | 200 / 200 ns | at timer resolution |
| `--navigation`: dormant windows / rows / estimated bytes | 2 / 150 / 328,052 B | 2 / 150 / 328,148 B | +96 B |
| Hover pass: first visits that opened without waiting | — | 12/12, 12 requests | new pass |
| Hover pass: requests for a 28-row sweep without clicks | — | 12 | capped at 12/min |

The baseline has no hover pass; there a first visit sends the same one request on click and waits
for it. In the hover pass synthetic responses arrive before the click, so it shows that a click
uses the preload without a second request, not how much latency is hidden; that depends on the
dwell and the network round trip. The three-conversation workload keeps two dormant windows on
both revisions, so the larger eight-window limit is not exercised here. The profile-extras cap
(2 s to 600 ms), image connection reuse and off-coordinator disk reads were not measured: they need
a network or disk-cache fixture this bench does not have.
## Embed styling review - October 14, 2026

Compared the original PR #650 head `c01461ed` with the review fixes in `9354335e` on
Windows 11 Home 10.0.26200, Ryzen 7 7800X3D and 33,410,678,784 bytes physical RAM.
Both standard unsigned portable packages were built with Rust 1.98.1,
`cargo xtask package` (`--release --locked --no-default-features`), including voice.
The installed-tree metric sums file lengths; both ZIPs use .NET `ZipFile` with
`CompressionLevel.Optimal`. Separate worktree `dist` directories were used. Baseline package
artifacts were later removed during a build-cache cleanup; the figures below were recorded
before that removal, and the changed portable tree was rechecked afterwards.

| Metric | Original PR head | Review fixes | Delta |
| --- | ---: | ---: | ---: |
| Package executable | 86,917,632 B | 86,925,824 B | +8,192 B (+0.0094%) |
| Installed portable tree | 91,110,921 B | 91,119,113 B | +8,192 B (+0.0090%) |
| Portable tree, ZIP | 50,316,564 B | 50,320,128 B | +3,564 B (+0.0071%) |

Release `profile_preview --demo --page=embeds --width=1120 --height=900 --interactive`
used the same synthetic fixture in both revisions, wgpu and 125% display scaling. Each
run had a five-second warmup and 20 one-second `Get-Process` samples. Memory is the observed
peak/final sample; CPU is process CPU time divided by elapsed wall time (100% = one core).
The console helper is separate. Other compilation was active during sampling, and there
is only one run per revision, so these small differences are not an improvement claim.

| Native process metric | Original PR head | Review fixes | Delta |
| --- | ---: | ---: | ---: |
| Preview peak / settled working set | 174,346,240 B | 174,428,160 B | +81,920 B |
| Preview peak / settled private bytes | 388,874,240 B | 389,730,304 B | +856,064 B |
| Preview idle CPU, one core | 0.162% | 0.405% | +0.243 percentage points |
| `conhost` peak / settled working set | 10,629,120 B | 10,670,080 B | +40,960 B |
| `conhost` peak / settled private bytes | 1,622,016 B | 1,740,800 B | +118,784 B |
| `conhost` idle CPU, one core | 0% | 0% | 0 percentage points |

Both release builds emit the existing OpenH264 `LNK4255` debug-info warning. NSIS is
unavailable on this host, so packaging skips the installer executable. These measurements
do not establish live Discord interoperability, startup latency or frame percentiles.
The [synthetic native fixture and reproduction procedure](pr-evidence/embed-styling-review/README.md)
describe the capture environment and package method. Dark/light and narrow native PNGs
were inspected. Manual native clicks and keyboard interaction were stopped by the user's
physical Escape key; source destinations and media actions have synthetic interaction tests.

## Profile cosmetics — October 11, 2026

Release builds of baseline `178bde35` and the cosmetics branch on Linux 7.2.9 (CachyOS),
AMD Ryzen 7 7800X3D, 30 GiB RAM, under headless gamescope at 1440 x 900 and scale 1.0.
Default features were used for the package; `--features demo` for the processes. Each run
had a 5 s warmup followed by 20 s sampled from `/proc/<pid>/status` at 1 s intervals; the
table shows the median of three runs. Both binaries were freshly copied first, so their
pages were equally cached. An earlier run against the just-linked branch binary showed
18 MiB more resident file pages from page-cache warmth. Heap and anonymous mappings were
identical in that run, so it is a measurement artifact.

| Metric | Baseline | Cosmetics | Delta |
| --- | ---: | ---: | ---: |
| Package executable | 87,314,600 B | 87,555,752 B | +241,152 B (+0.28%) |
| Installed tree (`xtask package --format dir`) | 91,848,634 B | 92,089,786 B | +241,152 B (+0.26%) |
| Installed tree, `tar` + `zstd -19` | 46,156,864 B | 46,280,238 B | +123,374 B (+0.27%) |
| `serein --demo` settled / peak RSS | 134 / 149 MiB | 134 / 149 MiB | 0 |
| `serein --demo` idle CPU (one core) | 0.25% | 0.25% | noise |
| `profile_preview --page=collectibles` settled / peak RSS | 94 / 110 MiB | 95 / 111 MiB | about +1 MiB |
| `profile_preview --page=collectibles` CPU | 0.25% | 0.25% | noise |

CPU did not change on the fixture page, which suggests the headless window was not playing
the effect (playback pauses while unfocused). Playback CPU was therefore not measured. Decoding two real Discord effect layers (450 x 880
APNG) through the worker's effect budget took 96–138 ms and produced 25–31 frames at
261 x 512. That is 13–16 MiB of frames per layer, against a 24 MiB layer budget and the
existing 128 MiB animation pool.

## GPU colour conversion for decoded video — October 11, 2026

Decoders now hand over native 4:2:0 planes (VideoToolbox NV12, GStreamer NV12/I420,
openh264 I420, Windows live NV12) and one render pass converts them on the GPU. Baseline
`178bde35` (main) and the changed tree were compared with Rust 1.98.1 on the same machines.
Native samples used the synthetic `--demo-video-playing` fixture replaced by a generated
clip for the run only, release builds with LTO off, 5–6 s warm-up, then 10 s of process CPU
time and peak RSS, alternating builds three times.

| Workload | Before | After | Delta |
| --- | ---: | ---: | ---: |
| macOS M1 Pro, 1080p30 H.264 High + AAC, inline playback CPU | 38.9% | 32.5% | −16% |
| macOS same run, peak RSS | 203.9 MiB | 177.0 MiB | −26.9 MiB |
| Asahi Linux (M1, Vulkan, labwc), 720p30 H.264 Baseline via openh264dec, CPU | 97.0% | 60.4% | −38% |
| Asahi same run, peak RSS | 294.9 MiB | 269.7 MiB | −25.2 MiB |
| openh264 decode + handoff, 120 alternating 1080p/720p frames (release, median) | 473.2 ms | 208.1 ms | −56% |
| openh264 shared scratch capacity | 8,294,400 B | 3,110,400 B | −62% |
| macOS standard executable (`cargo xtask package`) | 68,706,864 B | 68,723,648 B | +16,784 B |
| macOS dist directory / zipped app | 74,814,816 / 48,280,440 B | 74,831,600 / 48,288,340 B | +16,784 / +7,900 B |

Rendered colour bars matched VideoToolbox's own BGRA conversion byte for byte on macOS and
were identical on Linux. The macOS inline gain is smaller because VideoToolbox already
converted in hardware; Windows live NV12 previously converted per pixel on the CPU, but
Windows CPU, GPU memory and frame latency were not measured. Software numbers come from the
ignored `compare_alternating_software_decode` workload. Live calls and streams were not used.

## Long-session live memory inspection — October 9, 2026

Read-only `vmmap -summary`, `footprint` and `heap -s` inspected the owner's already-running
macOS process after 11 hours 33 minutes. No messages, calls, microphone use, UI interaction,
restart, payload dump or credential access occurred. Host: Apple M1 Pro, 16 GiB RAM,
macOS 27.0.1 (26A434). The installed executable's timestamp was October 7;
its source commit and enabled features were unavailable. It therefore appears to predate
main's October 8 idle-animation cleanup (`1b3e4a7b`), but this is not verified build provenance.

Physical footprint was 382.0 MiB, with a lifetime peak of 528.6 MiB. Ordinary `ps` RSS was
about 70.5 MiB; compressed/swapped private pages explain much of the discrepancy. The
697.2 MiB resident-region sum includes shared code/library pages and is not the app's
private footprint. The roughly 390 GiB virtual total is mostly reserved guard address
space, not RAM consumption.

| Allocation/category | Live snapshot | Interpretation |
| --- | ---: | --- |
| Allocated heap across malloc zones | 215.3 MiB | Includes Rust buffers and native objects; not all message state |
| Heap dirty/swapped space beyond allocated bytes | 42.7 MiB | Allocator fragmentation/free space, as reported by `vmmap` |
| Owned graphics backing | 84.4 MiB | Driver/GPU resources charged to the process |
| IOSurface | 26.9 MiB | Shared graphics surfaces |
| IOAccelerator graphics | 1.6 MiB | Additional mapped graphics resources |

These categories leave roughly 11 MiB for other footprint charges. Heap inspection found
205.7 MiB of untyped allocations, including hundreds of blocks around 208–800 KiB; this
is consistent with decoded image/frame sizes, not proof of their owner. Allocation stack
logging was not enabled, so exact attribution to Rust types/functions is unavailable.
One snapshot and a lifetime peak do not demonstrate a leak or a growth rate. Aggregate
sizes only are recorded here; no account data or heap payloads are published.
A second `footprint` sample at 11 hours 54 minutes reported 362 MiB (about 20 MiB less),
with graphics categories unchanged. The original production executable was still running;
no fix had been installed. Host builds were active, and app interaction was not controlled,
so this is an observation of ordinary variation, not an improvement from this PR.

Current main additionally keeps inline still textures in a 96-MiB / 384-item pool until
capacity eviction. The change expires renditions not painted for 60 seconds, using the
existing maintenance wakeup. Visible renditions refresh their deadline, while viewer-close
and five-second unplayed-frame cleanup retain their existing behavior. The pool's byte
accounting describes retained texture/frame resources, not whole-process memory.

Compared baseline `481a0c41` with this change on the same host, pinned Rust 1.98.1,
locked dependencies and release profile (fat LTO). The ignored `ui` library workload
`still_memory_workload` admits 160 synthetic 512×288 stills through the media library,
consumes upload deltas, then advances the maintenance clock by 61 seconds without painting.
It has no window, GPU, network or account. One warmup and three direct test-binary runs per
revision produced identical retained-byte counts:

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Peak retained inline texture bytes | 94,371,840 (90 MiB) | 94,371,840 (90 MiB) | 0 |
| Retained inline texture bytes after expiry | 94,371,840 (90 MiB) | 0 | −94,371,840 (−100%) |

These are texture-size accounting values, not measured GPU allocations or process RSS.
Regression tests additionally verify a renderer free delta, the exact expiry boundary,
painting renewal, reload eligibility and viewer-close cleanup. Native desktop automation
was not exposed in this session, so a scripted native populated-cache comparison was not
obtained. Native GPU reclamation, before/after process footprint, day-long soak, frame
latency and other platforms remain unmeasured. The production app was not replaced.

Both revisions passed standard `cargo xtask package` with voice included, no default
features and the locked release profile. Each app was saved separately; installed bytes
sum all app files, and distribution ZIPs use `ditto -c -k --sequesterRsrc --keepParent`.

| Package metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable | 68,657,520 B | 68,657,536 B | +16 B (+0.000023%) |
| Installed app | 74,695,855 B | 74,695,871 B | +16 B (+0.000021%) |
| Distribution ZIP | 48,255,410 B | 48,255,390 B | -20 B (-0.000041%) |

These package deltas are negligible. Both packages were locally ad-hoc signed, not notarized.

## Allocation-stack follow-up and idle artwork — October 10, 2026

The owner's restarted current-main build (`0d9c6772`, optimized release with
matching Rust symbols and `MallocStackLogging=lite`) was inspected after normal
server, image, profile, call and streaming activity. No agent-driven messages,
calls or microphone capture were performed. At roughly four minutes the process
footprint was 290.4 MiB (503.3 MiB peak), with 67.0 MiB live malloc allocations.
Graphics regions included 150.6 MiB owned unmapped backing, about 27 MiB IOSurface
and 1.6 MiB IOAccelerator. The malloc-zone report also included 69.6 MiB dirty
fragmentation/slack and 16.2 MiB profiling-tool data. These categories use
different accounting rules; they must not be added to derive footprint.

Allocation stacks identified 18.6 MiB for `ui::fonts::install_cjk` and 16.0 MiB
for epaint's CPU glyph atlas. The two large worker groups from the earlier
October 7 binary were absent. The previous 501.3-MiB and current 290.4-MiB live
captures had different activity and duration: they are diagnostic snapshots,
not a controlled improvement benchmark. Stack logging adds observer overhead.
Only addresses/stacks and summaries were captured, never allocation contents;
the sorted allocation report was capped at 16 MiB.

The leak scanner flagged 40,016 bytes in 440 blocks. Most belonged to macOS
AppIntents/XPC cycles; about 13 KiB traced to RNNoise's easyfft/generic_singleton
thread-local FFT planner cache. Its dependency deliberately uses `Box::leak`;
this small per-worker retention remains and is not fixed by artwork expiry.

Avatar/banner/activity/sticker/picker textures and custom/high-resolution emoji
textures previously stayed until their item/byte pools filled. They now expire
after 60 seconds without use, using the existing host maintenance deadline.
Visible and cached composer artwork refreshes the deadline. Composer layout only
reads cached still textures; it neither refreshes nor requests clipped assets.
Timeline custom/shared artwork and jumbo Unicode emoji resolve only for visible
slots, so leading overscan rows cannot retain or repeatedly reload them. Expired
artwork uses the normal bounded worker/disk-cache path when painted again.
Existing 64-MiB artwork and 16-MiB emoji ceilings are unchanged.

The ignored `avatars::tests::idle_artwork_workload` compares optimized release
unit-test builds with the pinned Rust 1.98.1 toolchain and lockfile on macOS
27.0.1 / Apple M1 Pro / 16 GiB RAM. The new harness was first run on unmodified baseline source before applying
the runtime change. The same workload submits 932 synthetic
64×64 RGBA textures without draining the existing 128-request batch, so only
128 avatar textures are admitted. It advances the maintenance clock by 61
seconds, with one warmup and three measured runs per build. All runs agree.
This measures retained texture payload accounting without a window or network,
not GPU allocation, process RSS, peak decoding memory or latency. Release samples
are reused from initial implementation `2b60dd8c`; the review follow-up changes
layout/paint access, not expiry accounting. One run of the final debug test binary
confirmed the same 2,097,152 B peak and zero settled payload. A redundant optimized
UI-test rebuild was stopped during compilation; it produced no new release sample.
Separate UI
regressions cover both cache pools, clipped/visible artwork, reloads and cached
composer image painting. Native before/after process and GPU measurements are
unmeasured; attempted standard-package `--demo` processes exited before a valid
native sample could be captured. Their exit cause was not established and no
zero-RSS result is used as evidence.

| Texture payload metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Peak admitted payload | 2,097,152 B | 2,097,152 B | 0 B |
| Payload after idle maintenance | 2,097,152 B | 0 B | -2,097,152 B / -100% |
| Retained cache entries after idle maintenance | 128 | 0 | -128 |

GPU drivers and allocators may reserve freed resources; these payload reductions
are not an equal whole-process RAM reduction guarantee. Returning after expiry
can re-decode cached artwork or fetch it again after a disk-cache miss. Font
storage and the shared Unicode emoji atlas retain their existing lifetimes.

The standard voice-inclusive package was rebuilt without demo/developer features.
The preserved baseline is the verified PR #618 package at `c16cb2df`, whose source
tree matches starting main `0d9c6772`; its executable hash was checked against
the baseline copy before comparison. This is a reused baseline, not a simultaneous
fresh package build. Both use the pinned release profile and lockfile.

| Package metric | Baseline | After | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| Executable | 68,657,536 B | 68,657,536 B | +0 B | Mach-O file length |
| Installed bundle | 74,695,871 B | 74,695,871 B | +0 B | Sum of bundle file lengths |
| ZIP | 48,255,390 B | 48,261,871 B | +6,481 B / +0.0134% | ditto -c -k --sequesterRsrc --keepParent |

The ZIP difference is small compression/signing noise, not a meaningful package
improvement. The local package is ad-hoc signed, not notarized. Full workspace
checking passes (1,234 tests, zero failures), including formatting, strict Clippy
and policy checks. Linux/Windows runtime behavior and a controlled live-account
soak after this new artwork change remain unmeasured.

## Gateway resume fallback — October 8, 2026

Compared baseline `2164f52e` with implementation `2ebc48a7` on Windows 11 x64,
AMD Ryzen 7 7800X3D (16 logical CPUs), 32 GB RAM, Rust 1.98.1 and the locked
dependencies. The recovery change adds one fixed-size failure counter, with no
new queue or persistence. Synthetic localhost tests now return to the original
gateway after three failed resume attempts; the baseline keeps selecting the
failed resume route. Manual recovery and successful resumes preserve their
existing behavior. These are protocol-policy checks, not live recovery timings.

Both revisions passed `cargo xtask package` with voice included, release fat LTO
and no default features. Each output directory was retained separately. Installed
bytes are the sum of all files in `dist`; archives use PowerShell
`Compress-Archive -CompressionLevel Optimal` over that directory.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable | 86,008,832 B | 86,010,368 B | +1,536 B (+0.0018%) |
| Full portable directory | 90,199,948 B | 90,201,484 B | +1,536 B (+0.0017%) |
| Portable ZIP | 50,002,123 B | 50,002,724 B | +601 B (+0.0012%) |

These are unsigned Windows portable packages. NSIS was unavailable, so no local
installer was produced. Both builds emitted the same OpenH264 duplicate-object
debug-info linker warning; packaging exited successfully.

The release reducer control used `cargo replay`, then one warmup and five direct
`replay-bench` runs per revision. It processed 100,000 events and retained 500
records / 331,992–332,477 estimated timeline bytes in both cases. Median times
were 159.8291 ms before and 117.5277 ms after. The five samples were
173.1020/159.8291/125.8932/152.4096/181.1192 ms and
128.2427/118.3857/106.1182/109.4493/117.5277 ms respectively. Other builds shared
the host, and the replay executables were byte-identical: this workload does not
include the changed gateway transport. The 42.3014 ms difference is not evidence
of a speed improvement. Process RSS, UI timing and live network recovery were
not measured.

## Animation frame retention — October 8, 2026

Decoded GIF and animated-avatar frames were the largest bounded RAM consumer. Each frame is held as
RGBA, so one 498x280, 40-frame GIF is about 22 MB and a 128 px, 40-frame avatar about 2.6 MB. Inline
GIFs kept their frames after scrolling away until a 128 MiB pool filled. Animated avatars and banners
kept theirs in another 128 MiB pool, although avatars only play on hover or in an open profile.

Frames not played for 5 s (scrolled away, not hovered, or shown in an unfocused window) are now
released while the still texture stays. Hover-only avatars keep frames only when they are about to
play. Released artwork asks the worker for frames again only once it can play; the encoded source comes from
the disk cache, so this costs a re-decode, not a download.

The ignored `ui` workload `animation_memory_workload` (release build, Apple M1, Rust 1.98.1, no window,
GPU, network or account) scrolls past 12 GIF embeds with 20 of 60 animated avatar rows on screen, then
settles for 6 s. Retained bytes were identical across three runs per revision (six on the change):

| Metric | Base | Change |
| --- | ---: | ---: |
| Peak retained decoded frames | 237.3 MiB | 111.7 MiB |
| Settled retained decoded frames | 237.3 MiB | 46.3 MiB |
| Peak process RSS | 265.7–265.8 MiB | 145.2–157.1 MiB (5 of 6 runs ≤ 145.4) |

Retained bytes come from the pools' own accounting. RSS did not fall after settling, because the
macOS allocator keeps freed ~0.5 MB blocks resident for reuse. Later decodes reuse them rather than
growing the process. GPU playback textures, which were also released, were not measured, and the
real app's scroll speed and media mix will differ. Standard macOS package: executable
68,526,000 B (+16,416), installed app 74,564,335 B (+16,416), `ditto` ZIP 48,183,136 B (+3,269).

## Active-call repaint cadence — October 8, 2026

A code audit of the UI, desktop wiring, client state and network/voice crates found one
always-on cost: while any call was active, `logic()` requested a repaint every 50 ms, running the
whole UI pass at 20 Hz even though speaking, notices, remote video, devices, hotkeys, screen share
and deadlines each already wake the UI themselves. The request is now a 1 s heartbeat.

The synthetic `--demo --demo-call` fixture (no credentials, no audio device, no network) was sampled
on an Apple M1 (16 GiB, macOS 27.0, Rust 1.98.1) with release `--features demo` builds of the base
commit and the change. Each sample waited 8 s, then summed process CPU time and polled RSS every
0.5 s for 30 s; base and change were alternated.

| Workload (30 s) | Base | Change |
| --- | ---: | ---: |
| Active-call fixture, CPU, 3 runs each | 6.27%, 5.77%, 6.17% | 0.50%, 0.50%, 0.50% |
| Active-call fixture, peak RSS | 126.3–126.5 MiB | 126.3–126.4 MiB |
| Plain `--demo` idle, CPU | 0.000% | 0.000% |

The 0.50% remaining is the 1 s heartbeat plus the call timer. CPU time has 10 ms resolution, so
treat the figures as approximate. They cover one fixture and display, not a live call: remote video,
screen share, audio threads and GPU work were not measured. A `footprint`/`heap` look at the idle demo
showed 68 MB physical footprint and 11.6 MB of live heap, so no idle-memory regression was found.

The same change set also avoids work that was not benchmarked, so no speedup is claimed for it:
permission decisions are no longer discarded when an event leaves the guild and channel records
equal or when an unrelated channel is removed, notification/read-state lookups use the indexed
channel map instead of a linear scan, and the composer thumbnail reads at most 64 MiB (the decode
allocation limit) instead of up to the 500 MB upload limit plus a second copy.

Standard no-default-features macOS packages built from both revisions: executable 68,509,584 B in
both, installed app 74,547,919 B in both, `ditto` ZIP 48,179,547 vs 48,179,867 B (+320 B).

## Native camera 15/30 fps selection (2026-11-08)

Compared the upstream `21f8870` camera implementation (fixed 15 fps) with this PR's
selectable-rate implementation using the same Logitech C270 at 640×480 on Nobara Linux 44
x86-64. The host used Rust 1.98.1, GNOME SDK 51 and an AMD Radeon RX 9070 XT. Each camera
run lasted eight seconds with a one-second warmup. The local probe counted frames and encoded
outputs only; it did not save video or connect to a call.

| Camera probe | Baseline | PR head | Change |
| --- | ---: | ---: | ---: |
| Selected 15 fps | 10.00 fps (68/69 encoded) | 14.86 fps (103/104 encoded) | +4.86 fps |
| Selected 30 fps | Not available | 29.66 fps (208/208 encoded) | New mode |

These are single local measurements, not sustained multi-run statistics or a guarantee of
network delivery. The optional physical probe is `crates/discord-voice/examples/camera_fps_probe.rs`.
Build it with:

```sh
cargo build --locked -p discord-voice -p platform --features winit/wayland --example camera_fps_probe
```

Then explicitly run `target/debug/examples/camera_fps_probe 15` and
`target/debug/examples/camera_fps_probe 30` on a machine where `/dev/video0` is the camera to
test. It starts local capture only, counts frames and encoded outputs, and saves or sends nothing.
It needs direct access to the camera device; Flatpak's default sandbox does not provide it.

Matching standard directory-format release packages were built separately from upstream `21f8870`
and the PR with `cargo xtask package --format dir`, Rust 1.98.1, GNOME SDK 51 and x86-64:

| Metric | Baseline | PR head | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 95,345,224 B | 95,346,760 B | +1,536 B |
| Installed directory payload | 100,044,456 B | 100,045,992 B | +1,536 B |

Compressed distribution size and camera CPU/RSS were not measured. The physical capture probe
does not test Discord call signaling or remote presentation; those remain owner-controlled live checks.

## Camera continuity during Gateway resume (2026-10-29)

Compared clean baseline `d93a8415406f7f36de5dc8b14e12ad0fc7134bb4` with runtime
source `839b019af0641768c04dc52bf5444607bf91283a` on CachyOS x86_64, using Rust
1.98.1 and `cargo xtask package --format arch`. Both used the standard voice-enabled
release build with no default features, fat LTO and one codegen unit. Each source
revision produced one measured executable and native package; the affected release
crates were rebuilt after switching source checkouts.

| Metric | Baseline | Fixed | Delta |
| --- | ---: | ---: | ---: |
| Executable bytes | 92,690,952 | 92,690,952 | 0 |
| Installed file payload bytes | 97,389,385 | 97,389,385 | 0 |
| Compressed Arch package bytes | 51,485,300 | 51,485,164 | -136 |

Executable and archive sizes use file lengths. Installed payload is the sum of regular
`usr/` file sizes in the package, excluding Arch metadata. Both packages passed the
native metadata, dependency version, payload, mode, ownership and no-script checks.
Default Debian packaging could not run because this host has no `dpkg`.

The compressed difference is packaging variation; no runtime speedup is claimed.
Camera/voice CPU, RSS, latency and live Discord behavior remain unmeasured. The change
adds no retained state, queues, dependencies or media processing. The affected
lifecycle has no equivalent camera-running baseline during an outage: baseline tears down
capture, while this change retains it. Comparing idle CPU or RSS at that point would compare
camera-off with camera-on rather than implementation overhead. A meaningful device/codec
resume-latency comparison requires controlled physical camera capture, which is not authorized
in this offline review session. The synthetic replay verifies lifecycle and command admission;
it does not measure media performance. No runtime performance improvement is claimed.

## Voice default-device polling — October 7, 2026

An offline probe compared creating a fresh PulseAudio client for every metadata poll with reusing
one client. Across five measured runs after warmup, 300 polls created 300 clients and left 300 peer
sockets connected in the fresh-client pattern; reuse created one client and left one socket. The
probe used synthetic socket pairs and a 2 ms pause per query. This identifies dependency resource
retention in the polling pattern; it does not establish application socket behavior or whole-process
memory use.

Both standard voice-enabled macOS packages were the same size: executable 68,180,992 B, installed
app 74,201,013 B, distribution 74,265,778 B. ZIP sizes differed by 37 B (48,039,907 vs 48,039,944 B),
which is packaging variation, not a runtime change. No Discord call, microphone, physical device,
callback latency, frame timing or CPU/RSS comparison was measured.

## CPU and memory audit — October 2, 2026

Release workloads compared a 100,000-event synthetic timeline, cursor scans, and synthetic video
frame ownership on an Apple M1 with Rust 1.98.1:

| Workload | Before | After | Result |
| --- | ---: | ---: | ---: |
| Reducer replay median | 153.985 ms | 53.504 ms | −65.25% |
| Full-history cursor query batch | 263.428 ms | 6.948 ms | −97.36% |
| Cursor batch with deleted tail rows | 341.132 ms | 16.075 ms | −95.29% |
| Coalesced 1080p frame peak RSS | 34.406 MiB | 26.516 MiB | −22.93% |
| Standard executable | 62,006,096 B | 62,006,112 B | +16 B |
| Installed bundle | 68,016,525 B | 68,016,541 B | +16 B |

The reducer retained 500 rows in both builds. Cursor timings isolate hot lookup work and do not
predict whole-UI gains. Frame conversion time was effectively unchanged; the lower peak RSS came
from reusing the undisplayed pending frame. With uploads every third frame, peak RSS was unchanged.

A focused native idle sample showed mean CPU of 1.180% before and 1.275% after, and settled RSS of
108.766 MiB and 111.484 MiB. This small, noisy sample supports no idle-performance improvement
claim. Startup latency, full-frame p95, GPU memory, live traffic and other platforms were unmeasured.

These workloads can be repeated with the pinned toolchain using `cargo replay` for the reducer and
the existing ignored client-core, desktop-frame and replay-soak workloads for detailed memory work.
The delivery skill documents how to compare a task baseline with the changed build. Do not compare
results from different machines or claim live-client behavior from synthetic fixtures.

## Voice participant controls (#592): Windows integration evidence - October 8, 2026

Fresh standard Windows x64 voice-enabled packages compare main `1b3e4a7b` with `d14edbb4` (measured 2026-10-08). Baseline/current file counts: 216/216.

| Metric | Main `1b3e4a7b` | Current integration | Delta |
| --- | ---: | ---: | ---: |
| Standard executable | 86,008,832 B | 86,089,216 B | +80,384 B (+0.0935%) |
| Installed directory | 90,199,948 B | 90,280,332 B | +80,384 B (+0.0891%) |
| Distribution ZIP | 50,001,798 B | 50,025,305 B | +23,507 B (+0.0470%) |

Method: `cargo xtask package`, Rust 1.98.1, standard release flags without demo; Windows 11 build 26200, Ryzen 7 7800X3D, 32 GiB RAM. Runtime workspace artifacts were invalidated before each feature build. Installed bytes sum every file in `dist`; ZIP uses whole-directory .NET Optimal compression. NSIS was unavailable, so no installer executable was built.

Alternating synthetic reducer replay (100,000 events): baseline median 76.5049 ms, range 74.0754-79.5180 ms; this change median 79.6071 ms, range 66.3717-83.4725 ms; median delta +3.1022 ms (+4.05%). One warmup each, then five baseline/controls/inbox rounds. All measured runs retained 500 records and 331,992-332,477 estimated timeline bytes (limit 4 MiB). The binaries were retained separately and workspace reducer artifacts rebuilt between variants. Concurrent compilation remained uncontrolled noise; overlapping ranges establish neither a performance improvement nor a regression.

Current native CPU, memory, frame/startup latency and affected-device behavior remain unmeasured because the native automation bridge is unavailable. Package size and synthetic reducer timing do not establish live Discord performance.

## Window geometry minimum — October 7, 2026

Baseline `c5e50e77` and fixed `c5ace4dd` were built on Windows 11 Home build
26200, Ryzen 7 7800X3D, 32 GiB RAM, Rust 1.98.1. Both standard voice-enabled
packages used `cargo xtask package` with no default or demo features. The installed
size sums all 216 files in `dist`; .NET `ZipFile.CreateFromDirectory` compressed
each complete directory with the same default settings. NSIS was unavailable, so
these measurements cover the package directory and ZIP, not an installer binary.

| Metric | Baseline | Fixed | Delta |
| --- | ---: | ---: | ---: |
| Release executable | 85,922,304 B | 85,922,304 B | 0 B |
| Installed package | 90,113,420 B | 90,113,420 B | 0 B |
| Distribution ZIP | 49,970,177 B | 49,970,277 B | +100 B (+0.00020%) |

For native idle samples, each source was also built with
`cargo build --release --locked -p serein --no-default-features --features demo`
and run with `--demo`.
Both runs used the same 1120×760 synthetic fixture, Windows DPI 120 (125% scale),
the default DX12 renderer and no child processes. After a 15-second warmup,
`Get-Process` sampled cumulative CPU time and private bytes every second for ten
seconds, with no further interaction. CPU is the mean one-core percentage; settled
private memory is the median of the final five readings.

| Native demo metric | Baseline | Fixed | Delta |
| --- | ---: | ---: | ---: |
| Idle CPU, 10 samples | 0% | 0% | Below sample resolution |
| Peak private memory | 192,835,584 B | 192,643,072 B | -192,512 B (-0.10%) |
| Settled private memory | 192,835,584 B | 192,643,072 B | -192,512 B (-0.10%) |

The tiny ZIP and memory differences do not establish a performance improvement.
Startup latency and frame timing were not measured. Demo ignores persisted geometry;
the offline window-geometry check covers restoration of undersized saved values.

## Window minimum and restoration (PR #583): Windows integration evidence - October 8, 2026

Fresh standard Windows x64 voice-enabled packages compare main `1b3e4a7b` with `371632f1` (measured 2026-10-08). Baseline/current file counts: 216/216.

| Metric | Main `1b3e4a7b` | Current integration | Delta |
| --- | ---: | ---: | ---: |
| Standard executable | 86,008,832 B | 86,009,344 B | +512 B (+0.0006%) |
| Installed directory | 90,199,948 B | 90,200,460 B | +512 B (+0.0006%) |
| Distribution ZIP | 50,001,798 B | 50,002,093 B | +295 B (+0.0006%) |

Method: `cargo xtask package`, Rust 1.98.1, standard release flags without demo; Windows 11 build 26200, Ryzen 7 7800X3D, 32 GiB RAM. Runtime workspace artifacts were invalidated before each feature build. Installed bytes sum every file in `dist`; ZIP uses whole-directory .NET Optimal compression. NSIS was unavailable, so no installer executable was built.

Current native CPU, memory, frame/startup latency and affected-device behavior remain unmeasured because the native automation bridge is unavailable. Package size and synthetic reducer timing do not establish live Discord performance.

### Window minimum after monitor changes — review follow-up (2026-10-08)

Fresh standard Windows x64 voice-enabled packages compare `666d7d4a` with this
review fix. Both use Rust 1.98.1, the pinned lockfile, standard release flags
without demo, Windows 11 build 26200, Ryzen 7 7800X3D and 32 GiB RAM. Each package
contains 216 files. Installed bytes sum all files in `dist`; ZIP uses the complete
directory with .NET `ZipFile.CreateFromDirectory`, Optimal compression.

| Metric | Before | After | Delta |
| --- | ---: | ---: | ---: |
| Standard executable | 86,009,344 B | 86,011,904 B | +2,560 B (+0.0030%) |
| Installed directory | 90,200,460 B | 90,203,020 B | +2,560 B (+0.0028%) |
| Distribution ZIP | 50,002,090 B | 50,003,472 B | +1,382 B (+0.0028%) |

Both `cargo xtask package` runs passed; NSIS is unavailable, so no installer
executable was produced. The final `cargo xtask check` passed formatting, strict
workspace Clippy, workspace tests, demo compilation and policy checks. Four
focused application-settings tests passed, including small displays, scale and
decoration bounds for the recalculated minimum.

Native capture/interaction remains unavailable (the native helper reports a
missing pipe). Moving a real window between monitors, native CPU/memory, startup
and frame latency were not measured. These package measurements and synthetic
tests do not establish native multi-monitor or live Discord behavior.
## Watched-stream recovery — October 8, 2026

Standard Windows x64 voice-enabled packages (`cargo xtask package`, no demo feature,
Rust 1.98.1) compared baseline `1b3e4a7b` and fixed runtime `5b446a45` on Windows 11
build 26200, Ryzen 7 7800X3D, 32 GiB RAM. Executable size stayed 86,008,832 B;
the 216-file installed directory stayed 90,199,948 B. Whole-directory .NET Optimal
ZIP size changed from 50,001,798 B to 50,001,523 B (-275 B), packaging variation.
NSIS was unavailable, so no installer binary was built. Native CPU/memory/frame
measurements and live stream continuity remain unmeasured because the native
automation bridge is unavailable. The lifecycle regression checks retained worker
ownership during recovery; it does not measure network quality or throughput.

## Channel and message pills — October 7, 2026

Compared baseline `38d919d7` with runtime `78de8d15` on Windows 11 Home
10.0.26200, Ryzen 7 7800X3D (16 logical CPUs), 33,410,678,784 bytes RAM,
Rust 1.98.1. Both use the same new synthetic channel-links fixture and the
same Windows allocator lockfile correction, required to compile the baseline.
The release preview uses WGPU with the demo feature and 125% display scale;
the selected GPU/backend was not logged.

After five seconds warmup, 21 process-counter samples were taken about one
second apart per build. This was a static idle fixture, with no scripted input.
Baseline elapsed sample time was 20.536 s and after was 20.252 s. Settled memory
is the mean of the final five samples.

| Metric | Baseline | After | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| CPU, percentage of one core | 0.00% | 0.31% | +0.31 percentage points | Windows process CPU delta / actual elapsed time |
| Peak / settled working set | 196,276,224 B | 198,307,840 B | +2,031,616 B / +1.04% | Windows WorkingSet64; maximum / mean of last five samples |
| Settled private bytes | 414,261,248 B | 415,698,944 B | +1,437,696 B / +0.35% | Windows PrivateMemorySize64; mean of last five samples |

One series per build on a shared machine with compiler activity is observational;
it supports no performance improvement claim. Native frame/startup latency and
live-service performance were not measured. Both standard voice-inclusive release
packages passed, without demo/developer features:

| Package metric | Baseline | After | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| Executable | 85,449,728 B | 85,464,064 B | +14,336 B / +0.017% | serein.exe file length |
| Installed files | 89,623,341 B | 89,637,677 B | +14,336 B / +0.016% | Sum of all dist file lengths |
| ZIP distribution | 49,775,655 B | 49,781,438 B | +5,783 B / +0.012% | Compress-Archive, Optimal; ZIP file length |

Installed bytes sum all files in `dist`; ZIPs use PowerShell `Compress-Archive`
with Optimal compression. NSIS was unavailable, so no installer executable was
generated locally. Both unsigned packages emitted the existing OpenH264
duplicate-object debug-info linker warning.

Inspected dark/wide and light/narrow images are eframe framebuffer exports. The
native computer-use helper was unavailable, so OS screenshots, native input and
screen-reader operation remain unverified. Workspace checking reached 1,154
passing tests, eight failures also reproduced on the baseline, and 23 ignored
tests. Seven focused pill tests and strict Clippy passed after the final visual
correction. Commands, raw counters, baseline failure names and image provenance
are in [the evidence directory](pr-evidence/channel-message-pills/README.md).

### Review follow-up package

The standard voice-inclusive package was rebuilt at `7d9c0d05` after the review
fixes, using the same Windows host, pinned toolchain, no demo/developer features,
and compression method. It passed; NSIS remains unavailable locally. Comparison
with the same `38d919d7` baseline:

| Package metric | Baseline | After review | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| Executable | 85,449,728 B | 85,464,576 B | +14,848 B / +0.017% | serein.exe file length |
| Installed files | 89,623,341 B | 89,638,189 B | +14,848 B / +0.017% | Sum of all dist file lengths |
| ZIP distribution | 49,775,655 B | 49,781,661 B | +6,006 B / +0.012% | Compress-Archive, Optimal; ZIP file length |

Raw sizes and the executable hash are in
[review-package.json](pr-evidence/channel-message-pills/review-package.json).
The process samples and screenshots above remain evidence for `78de8d15`; they
were not repeated for the review fixes and support no claim about changed CPU
or memory usage. Full workspace checking at `f9e568c9` reached 1,157 passes,
the same eight baseline failures, and 23 ignored tests. After the final Chinese
label correction, all ten focused pill tests, formatting and strict workspace
Clippy passed. Native input/capture remain unverified; merge blockers persist.

### Reply message-link preview follow-up (2026-10-08)

Compared `d2299412` with the reply-preview fix, using the new synthetic
`channel-link-replies` fixture in both builds. The Windows host, WGPU renderer,
125% scale and five-second warmup match the previous section. Each build has one
series of 21 process-counter samples over about 20 seconds. Baseline and changed
preview executables were rebuilt and preserved separately. Concurrent builds
and OS residency add noise; these idle results are not a performance improvement
claim and do not measure frame latency or live Discord behavior.

| Metric | Baseline | After | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| CPU, one-core percentage | 0.692% | 0.000% | -0.692 percentage points | Process CPU delta / elapsed time |
| Peak working set | 200,089,600 B | 213,258,240 B | +13,168,640 B / +6.58% | WorkingSet64 maximum |
| Settled working set | 200,089,600 B | 213,257,421 B | +13,167,821 B / +6.58% | WorkingSet64 mean of last five samples |
| Settled private bytes | 416,260,096 B | 416,219,136 B | -40,960 B / -0.010% | PrivateMemorySize64 mean of last five samples |

Working set increased while private bytes were approximately flat. Raw samples,
the inspected light/dark framebuffer pairs and reproduction details are in
[the reply evidence](pr-evidence/channel-message-pills/README.md#reply-message-links-2026-10-08).
Native input and OS screenshot capture remain unavailable; a successful
framebuffer export does not establish them.

The fresh standard voice-inclusive package passes without demo/developer
features. Compared with the historical `7d9c0d05` package, executable and
installed payload grow by 512 B; ZIP grows by 214 B. The runtime source at
`7d9c0d05` is identical to pre-fix `d2299412` (the intervening changes are four
documentation files), but this is not a fresh simultaneous package baseline.

| Package metric | Historical baseline | Reply fix | Delta | Method |
| --- | ---: | ---: | ---: | --- |
| Executable | 85,464,576 B | 85,465,088 B | +512 B / +0.0006% | serein.exe file length |
| Installed files | 89,638,189 B | 89,638,701 B | +512 B / +0.0006% | Sum of dist file lengths |
| ZIP distribution | 49,781,661 B | 49,781,875 B | +214 B / +0.0004% | Compress-Archive, Optimal; ZIP file length |

[reply-package.json](pr-evidence/channel-message-pills/reply-package.json) records
the new measurements and executable hash. NSIS remains unavailable; this is an
unsigned directory and ZIP. Final serial workspace checking reached 1,158
passes, eight recorded baseline UI failures and 23 ignored tests. Formatting,
strict workspace Clippy and the independent policy check passed.

## Post rename review repair (2026-10-14, PR #655)

Fresh standard voice-inclusive packages compare pre-repair `74ff7038` with
runtime repair `460d7117`; both include the same main integration (`c84b5998`).
Environment: Windows 11 x64 build 26200, Ryzen 7 7800X3D, 32 GB RAM, pinned
Rust 1.98.1. `cargo xtask package` builds without default, demo or developer
features. NSIS is unavailable, so these are unsigned directory packages and
ZIP archives, not installer validation.

| Metric / method | Baseline | Repair | Delta |
| --- | ---: | ---: | ---: |
| serein.exe file length | 87,251,456 B | 87,251,456 B | 0 B |
| Installed payload, 216 files | 91,444,745 B | 91,444,745 B | 0 B |
| ZIP, sorted paths / DEFLATE level 9 | 50,287,594 B | 50,287,599 B | +5 B |
| 100,000-event reducer replay, median of five | 68.47 ms | 66.50 ms | -1.98 ms / -2.89% |

Each replay executable was built once and verified through `cargo replay`,
then run directly with one warmup and five measured samples. Both retain
500 timeline records / 393,492..393,977 estimated bytes. The timing ranges
overlap (67.82..73.78 ms before, 64.52..70.47 ms after); this is not evidence
of a speed improvement. The workload measures the general synthetic reducer,
not rename-specific latency, process RSS, UI frame timing or live compatibility.

The repair retains existing bounded state and adds no dependency or cache.
Regressions cover revoked access, rename echoes with invalidated mutable details,
a fresh post-details reload and a later conflicting name. Full local
`cargo xtask check` and separate fuzz formatting passed. Native GUI capture
and owner-controlled live validation remain unavailable/unverified.
Raw samples, source revisions and executable hashes are in
[measurements.json](pr-evidence/post-rename-review/measurements.json).

## URL markup parsing (2026-10-14, issue #656)

[Measurements and source hashes](pr-evidence/url-underscores/measurements.json)
compare `fbcbbad3` with the URL protection change on Windows 11 x64 build 26200,
Ryzen 7 7800X3D, 32 GB RAM and pinned Rust 1.98.1. Standard packages include voice
and use `cargo xtask package`, without demo or developer features. Performance
notes and development evidence are not included in the installed payload.

| Metric / method | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable | 87,366,656 B | 87,381,504 B | +14,848 B / +0.0170% |
| Installed payload, 216 files | 91,559,945 B | 91,574,793 B | +14,848 B / +0.0162% |
| ZIP, Optimal compression | 50,470,238 B | 50,479,398 B | +9,160 B / +0.0181% |
| Plain message, median / 10,000 parses | 25.1799 ms | 26.1144 ms | +0.9345 ms / +3.71% |
| Underscore URLs, median / 10,000 parses | 28.7708 ms | 41.5100 ms | +12.7392 ms / +44.28% |

The plain-message sample ranges overlap; no speed improvement is claimed.
Protecting underscore URLs adds about 1.27 microseconds per parse in this workload.

The release component workload is `cargo test --release --locked -p ui --lib
url_underscore_parse_benchmark -- --ignored --nocapture`. Each built test binary
is then run directly, with one warmup and five samples of 10,000 parses for each
synthetic message. This measures parsing, not native frame latency, idle CPU,
RSS or live Discord behavior. Native desktop automation is unavailable here.

These numbers were measured on the underscore-only revision `e6f92b0f`. Protection
was later generalized to `*`, `~` and `&` entities, plus a trailing-opener check of
at most four passes. That revision was not re-measured.

Protection adds temporary passes only for candidate URLs containing `_`, `*`, `~`
or `&`; other messages use an early return. Source remains capped at 8 KiB, each
pass at 512 events (exhaustion falls back to bounded literal text), and stored
links at 16. No dependency or retained cache is added.


## Review batch integration (2026-11-01)

Compared baseline `c37a4f3e` (the same source tree as main `5caf787a`) with
combined source `b9642a03`. The baseline already contains #860, #843 and #859;
this comparison covers the remaining seven PRs together. It cannot attribute costs
to one PR. [Source hashes and samples](pr-evidence/review-batch-20261031/measurements.json)
record Windows 11 x64, pinned Rust 1.98.1 and the host CPU/RAM. Both standard
`cargo xtask package` builds include voice and disable developer/default features.
NSIS is unavailable, so these are unsigned portable payloads without an installer.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable bytes | 92,469,248 | 92,695,552 | +226,304 / +0.245% |
| Installed payload bytes | 96,828,010 | 97,054,749 | +226,739 / +0.234% |
| ZIP bytes (Deflate level 9) | 52,875,112 | 52,959,631 | +84,519 / +0.160% |
| Reducer median, 100000 messages | 72.0211 ms | 71.1823 ms | -0.8388 ms / -1.16% |

The installed file counts are 233 and 233. ZIPs use sorted relative paths
and Python ZIP_DEFLATED at level 9. PR images and measurement artifacts are not installed.

Reducer binaries were built once with `cargo build --release --locked -p replay-bench`,
then alternated after builds finished: one warmup and five samples per binary. The
100000-message workload checks 500 retained records and a bounded estimated timeline.
It is a general reducer check, not an avatar-burst, input-latency or camera benchmark.
Background OS load is not controlled; no speed improvement is claimed. Process RSS,
UI frame latency, physical audio/camera behavior and live Discord remain unmeasured.

### UHD inline playback — November 6, 2026

Inline attachment playback now shares the live bound: 3840×2160 worth of pixels within a
3840-pixel square, so portrait and ultrawide videos fit as well. Compressed and YUV samples
keep their 16 MiB cap; Windows' RGB frames get a separate 40 MiB cap covering a 4K frame
with aligned surface padding. The player still queues two decoded frames, at most about
25 MiB as 4K NV12 or 64 MiB as RGBA; 1080p and smaller videos allocate as before. Frames
larger than the renderer's maximum texture side are refused by the CPU fallback. The
FFmpeg transcode fallback still produces at most 1080p. No 4K playback CPU or memory
measurement has been recorded yet.

## Graphics API picker — November 6, 2026

Baseline `354d033b31e89079e50c16684852fd2b039c225a` versus the picker implementation in
this PR. Ubuntu 24.04 x86_64 in Docker, pinned Rust 1.98.1 and lockfile, unchanged
standard voice-enabled fat-LTO release profile. Both `cargo xtask package` runs
passed the DEB smoke checks: executable, metadata, ownership, contents and host
shared-library closure. Workspace artifacts were cleared for the relevant build
profile before switching source worktrees; third-party artifacts were reused.

| Metric / method | Baseline | Picker | Delta |
| --- | ---: | ---: | ---: |
| Standard executable | 93,979,096 B | 94,000,216 B | +21,120 B (+0.022%) |
| Installed regular files (245 files) | 98,678,328 B | 98,699,448 B | +21,120 B (+0.021%) |
| Compressed DEB | 47,292,820 B | 47,296,900 B | +4,080 B (+0.009%) |

Installed bytes sum regular files extracted from the unsigned DEB; they are not
filesystem allocation or RAM. One release package was built per revision. These
small byte changes establish no runtime performance improvement.

Supplementary native X11/Xvfb samples use **debug demo builds**, not the standard
release binaries. Intel Core i5-1235U, 30 GiB visible RAM, llvmpipe (LLVM 20.1.2)
through Vulkan, scale 1.0, 1280 × 950 window, General scrolled to its bottom with
Automatic selected and the picker closed. One 10-second warmup and one 20-sample
run per revision, one-second sampling.

| Native debug demo metric | Baseline | Picker | Delta |
| --- | ---: | ---: | ---: |
| Idle CPU (100% = one core) | 0.10% | 0.10% | Below 0.01 percentage-point reporting precision |
| Peak / settled sampled process RSS | 349,192,192 B | 349,691,904 B | +499,712 B (+0.143%) |

CPU uses process user/system tick deltas over elapsed time; RSS uses `/proc`'s
`VmRSS`. Both runs lasted about 20.007 seconds. Concurrent compilation is
uncontrolled noise; display-server/compiler CPU and memory were not sampled.
No release CPU/RSS, p95 frame/startup latency, physical GPU speed or affected
Windows hardware behavior was measured. No Discord session or live media was used.
See [the native evidence and reproduction procedure](pr-evidence/render-api-picker/README.md).

## Read ACK reconciliation — November 6, 2026

Baseline PR #908 head `449211db` versus the final Rust sources in the follow-up containing
creation-time service-contribution evidence, profile-independent ignored status, and the
`--ack-reconciliation` replay mode.
Ubuntu 24.04.5 x86-64, Intel i5-1235U, Rust 1.98.1, unchanged dependency versions, identical
release settings and benchmark harness. The baseline's isolated workspace prunes unrelated
lockfile entries; its dependency name/version/source triples were checked against the repository
lockfile. Neither repository lockfile nor dependency versions change in this PR.

Run `cargo replay --ack-reconciliation`. Each process constructs eight independent full ledgers
for partial reads and eight for bulk reads, each with 4,096 observed messages across 1,024
private channels. Timing includes only HTTP-result reduction, excluding fixture construction and
request preparation. Each bulk result confirms all 1,024 channels. Warm each binary once, then
run five counterbalanced pairs (AB, BA, AB, BA, AB) with no concurrent container builds or
packaging. Pilot runs showed substantial host timing noise; this final series uses the
counterbalanced order rather than interpreting an A-first ordering bias as an improvement.

| Timed work per process | Baseline median (range) | After median (range) | Median delta |
| --- | ---: | ---: | ---: |
| Eight partial confirmations | 0.115081 ms (0.111414–0.143944) | 0.117672 ms (0.111691–0.176175) | +0.002591 ms (+2.3%) |
| Eight 1,024-channel bulk confirmations | 43.083 ms (40.419–64.171) | 42.706 ms (40.043–74.701) | −0.377 ms (−0.9%) |

Ranges overlap substantially; neither a speedup nor an absence of regression is established.
The values are not universal latency results. These workloads exercise the added ledger scans and
bulk reconciliation, unlike the earlier message-only replay. No speedup, RSS, UI timing, live
Discord compatibility, or message-creation classification throughput is established by them.
The ledger keeps its 4,096-row / 128-KiB bound. The PR records the measured runtime commit;
subsequent documentation-only changes do not affect these Rust sources.

## Mention shortcuts — November 6, 2026

Baseline `62df2c92f839c4329cc9e7d05052afa599a61bb5` and runtime source
`363b7e6bf27ccb1b82a966aa81fe2f35e746cf05`, rebuilt in separate worktrees and output
directories. Ubuntu 26.04.1 x86-64, Rust 1.98.1, pinned lockfile, release optimization,
voice included, `--no-default-features`, `CARGO_BUILD_JOBS=1`, and
`CARGO_PROFILE_RELEASE_LTO=false` on both revisions. Each build ran alone in a cgroup
with `MemoryMax=8 GiB` and `MemorySwapMax=0`, respecting the owner's 16 GB system budget.

One package per revision. Executable/archive sizes use `stat`; installed payload sums
regular-file sizes from `dpkg-deb --fsys-tarfile`, and Installed-Size comes from Debian
control metadata. Both packages passed the existing 245-file payload, metadata,
ownership, desktop-entry and host shared-library closure checks. No packages were
installed and no authenticated client was launched for these measurements.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable (bytes) | 96,549,240 | 96,608,376 | +59,136 (+0.061%) |
| Installed payload (regular-file bytes) | 101,248,472 | 101,307,608 | +59,136 (+0.058%) |
| Debian Installed-Size (KiB) | 99,077 | 99,135 | +58 (+0.059%) |
| Compressed .deb (bytes) | 47,427,752 | 47,451,612 | +23,860 (+0.050%) |

The standard fat-LTO attempt reached the enforced 8-GiB build cap and was stopped
by the kernel. The successful local packages use the explicit LTO override above;
these size results apply to that build setting. The source profile and dependency
versions are unchanged. Later commits add tests/documentation only.

Native computer use exposed no applications in this session. Native before/after
screenshots, UI workload warmup/sampling, CPU/RSS, display scale/renderer observation,
frame latency and live Discord verification remain unavailable. No UI speed or
memory improvement is established by these package sizes.

## Browser transport identity (PR #904) — November 10, 2026

Baseline `6e6f18b0` (main, reqwest 0.13.5 over rustls/AWS-LC) and source `1ab16ae0`
(wreq 6.0.0-rc.31 over BoringSSL), each built once with `cargo xtask package` in its own
worktree and a new target directory. macOS 27.2 arm64, Apple M5 Pro / 24 GiB, Homebrew Rust
1.99.1, pinned lockfile, standard release profile including voice.

Executable and archive sizes use `stat`; the bundle size is `du -sk dist/Serein.app`; the
archive is `ditto -c -k --keepParent dist/Serein.app`. No client was launched and nothing
contacted Discord.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Executable (bytes) | 76,200,032 | 75,296,368 | −903,664 (−1.19%) |
| `Serein.app` on disk (KiB) | 81,072 | 80,200 | −872 (−1.08%) |
| Zipped bundle (bytes) | 52,158,053 | 51,734,674 | −423,379 (−0.81%) |
| Clean `cargo xtask package` (s, one sample) | 282 | 275 | −7 |

The build times are single samples and other work ran on the machine during the baseline,
so they show only that the BoringSSL build does not lengthen a clean macOS package build
noticeably. Linux, Windows and Windows ARM64 (BoringSSL without assembly) sizes were not
measured. Connection setup time, TLS throughput and memory were not measured.

## REST admission verification — November 12, 2026

Baseline `acccd59e`, Fedora Asahi Remix 44 aarch64, pinned Rust 1.99.1,
standard voice-inclusive package with no default features. The normal fat-LTO
release link exceeded the local agent group's 11.45-GiB memory cap, including
with one Cargo job; kernel logs recorded OOM kills. No repository release-profile
setting was changed.

One baseline-only `CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_LTO=false cargo xtask
package --format dir` completed: executable 89,101,464 bytes, apparent installation
tree 93,814,166 bytes (`du -sb`), deterministic gzip-6 tar 57,148,868 bytes
(sorted members, zero timestamps/ownership). These are not standard fat-LTO
release measurements. The owner stopped further builds before an after package
was produced, so no package-size, latency, CPU or RSS comparison is available
and no performance improvement is claimed.

The admission tracker has a compile-time 16-KiB ceiling for its two vector backing
allocations and at most 128 entries per vector; these are bounds, not process-RSS
measurements. Same-bucket requests are serialized; exhausted successful responses
hold only their route/bucket, while 429 responses retain the existing conservative
shared cooldown. Synthetic regressions cover independent-channel progress,
queued verification expiry, aliases, major resources, eviction and terminal wakeup.

## `interface_ui` extension layout — November 17, 2026

Baseline `789d8300`, task branch `feat/ventrilo-extension` (after commit recorded in the PR).
Windows 11 build 26200, AMD Ryzen 5 7600X (12 threads), 32 GiB RAM, 3840×2160 at 100% scale,
wgpu default backend, pinned toolchain and lockfile. Measured with an external plugin that uses
the generic `interface_ui` capability to replace the main panes with a declarative tree of text
and voice channels and voice participants; no window shows beside the tree until a channel is
double-clicked.

Idle process samples of the native `--demo --demo-voice` fixture: 25 s warmup, then 30 samples
at 1 s intervals; CPU is process processor-time delta over wall time, memory is working set and
private bytes. Matching debug demo builds are used because release packages exclude the `demo`
feature; the after build loads the seeded plugin (layout on), the baseline has no plugin.

| Metric | Baseline | After | Delta |
| --- | ---: | ---: | ---: |
| Release `dist/serein.exe` (bytes) | 99,460,096 | 99,506,688 | +46,592 (+0.047%) |
| Idle CPU (%) | 0.42 | 0.26 | −0.16 pp |
| Peak working set (MiB) | 217.5 | 220.9 | +3.4 |
| Settled working set (MiB) | 217.5 | 220.9 | +3.4 |
| Peak private bytes (MiB) | 274.4 | 257.8 | −16.6 |

Single-run samples on a shared desktop: deltas of this size are noise, not improvements or
regressions. Contended samples were discarded and re-measured on an idle desktop: one baseline
(11.51% idle CPU) and two after runs (18.91%, and 5.47% while a screenshot capture ran
concurrently). Frame latency and startup time were not instrumented and are not claimed.

## `start_screen` player — November 17, 2026

The host no longer rasterizes anything for a plugin start screen and runs no render thread for
it: the plugin ships pre-rendered 2-bit frames and the host only plays them back. When a screen is
loaded or accepted, its frames are decoded once into one palette-index byte per cell (at most
655,360 cells for the 160 KiB packed limit). While the connecting screen shows, a displayed
frame change maps at most 160×160 indices per image through its palette into one reused
nearest-filtered texture (one 100 KiB RGBA upload per changed image, at most four images), and
each animation asks for a repaint only at its next frame boundary, at most 30 per second. A
still image uploads once; nothing runs once the screen is gone.

Throwaway timing harness, Windows 11 build 26200, AMD Ryzen 5 7600X (12 threads), 32 GiB RAM,
copying the player's decode and palette loops (rustc 1.99.1), 25 frames of 160×160 cells
(160,000 packed bytes):

| Build | Decode once (640,000 cells) | Map one 160×160 frame | Four images at 30 fps, one core |
| --- | ---: | ---: | ---: |
| Release | 0.96 ms | 0.030 ms | 0.36% |
| Debug | 14.5 ms | 0.24 ms | 2.9% |

Single runs on a shared desktop. Texture upload, GPU time, frame latency and whole-process CPU
were not instrumented and are not claimed; other screens pay nothing for the feature.
