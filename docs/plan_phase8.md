# Plan: Phase 8 (any machine is a server, runs VMs, works without a static IP)

Research behind this plan (existing tools, VM options, dynamic IPs, what comes next): [research_home_hosting.md](research_home_hosting.md).

Status: **proposed** (2026-11-04). This plan follows [Phase 7](plan_phase7.md), the [public roadmap](public_roadmap.md) and the [web control plane roadmap](roadmap_web_control_plane.md). Sizes: **S** ≈ days, **M** ≈ 1–2 weeks, **L** ≈ several weeks. Every item ships on its own, off by default, with a "done when" check, and starts as 🧪 Experimental in [feature_maturity.md](feature_maturity.md) until CI backs it.

Principles (unchanged): one binary, embedded SQLite, no required external services; every change is an audited event; nothing is called validated without a CI test.

---

## 1. Where Aegis stands (audit of `main`, 2026-11-04)

**Done:** deploy pipeline with health checks and automatic rollback, supervisor, HTTP API, auth and roles, TLS and ACME (HTTP-01 only), built-in reverse proxy with zero-downtime cutover, dashboard, live cgroup limits, secrets, backups, self-update, Docker runtime, instances with Rolling/BlueGreen, jobs, live URL check, Prometheus, per-user app lists, storage cleanup.

**Fixed since the last audit:** the supervisor fd leak (commit `4273822`, "Close a process's log file when it stops"); the nightly 60-minute soak has passed since.

**Open from Phase 7:** per-app API tokens, TOTP, OIDC (M1); deploy hooks, approval gates, canary, preview environments (M2); more alert channels and alert rules (M3); S3 backups and cloud integrations (M4); add-ons, buildpacks, scale to zero (M5); fleet (M6).

**Known gaps that matter for this plan:**

| Gap | Where | Why it matters |
| :--- | :--- | :--- |
| No soak longer than 60 minutes (the fd leak the 15-min soak found is fixed, and the nightly 60-min soak passes) | [benchmarks.md](benchmarks.md) | An always-on home server needs evidence for weeks, not an hour. |
| macOS and Windows are built but never tested | [compatibility.md](compatibility.md) | A laptop is often a Mac or a Windows machine. |
| ACME only does HTTP-01 | `runtime/ingress/src/acme.rs` | Needs inbound port 80. Home networks and CGNAT usually can't offer it. |
| `core/ssh`, `core/state`, `git/action`, `integrations/github`, `deployment/analytics` are stubs | [feature_maturity.md](feature_maturity.md#not-implemented-yet) | Fleet and "add a server by SSH" depend on `core/ssh`. |
| Only Node.js is deploy-tested | [compatibility.md](compatibility.md) | Python, Go and Rust users hit untested paths first. |

## 2. What the field looks like (research summary)

| Tool | What people pick it for | What Aegis can learn |
| :--- | :--- | :--- |
| **Coolify** | Widest feature set: hundreds of one-click services, Docker Compose, many Git providers (GitHub, GitLab, Bitbucket, Gitea, Forgejo), multi-server | Templates and Compose are table stakes. Coolify had 11 vulnerabilities disclosed in January 2026, 3 rated CVSS 10.0, so a small, audited, single-binary design is a real selling point. |
| **Dokploy** | Lighter, Docker-first; Nixpacks/Dockerfile/Compose, databases, Swarm multi-server | Buildpacks (Nixpacks) and managed databases are expected. |
| **Kamal 2** | No dashboard, SSH-driven; `kamal-proxy` switches traffic only after `/up` is healthy; remote builders; "accessories" (databases, caches) | Aegis already has the proxy and health gating. Remote and multi-arch builds and accessories (≈ add-ons) are the gaps. |
| **CapRover, Dokku** | Mature and simple, Heroku-like `git push` | `git push aegis main` as a deploy path. |
| **Cloudflare Tunnel, Tailscale (Funnel), frp, ngrok** | Reach a home machine behind NAT or CGNAT without opening a port: the daemon dials *out* and holds the connection | The basis for "turn a laptop into a server" (§3). |

None of these turns an ordinary laptop into a server for you: they all assume a VPS with a public IP. **That is the gap Phase 8 targets first.**

---

## 3. Milestone H: Home server mode (turn any laptop or PC into a server) — L

**Goal:** someone with an old laptop, a desktop or a mini PC runs one command, and it becomes a server that stays on, survives reboots and power cuts, is reachable from the internet over HTTPS (even behind CGNAT), and tells them when something is wrong (battery, heat, disk, network).

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | sudo bash -s -- --server --home
# or, on an existing install
aegis host prepare        # checks and fixes, shows a report first, asks before each change
aegis host status         # power, battery, temperature, network, reachability
```

The dashboard gets a **Host** page (admins only) with the same checks as cards: green, amber or red, each with a **Fix** button and a "what this changes" note.

### H1. Host readiness check (`aegis host check`) — S

Read-only. Reports what would stop this machine from being a good server, with a fix for each:

| Check | Linux | macOS | Windows |
| :--- | :--- | :--- | :--- |
| Sleeps when idle | `systemctl is-enabled sleep.target`, logind `IdleAction` | `pmset -g` | `powercfg /query` |
| Sleeps when the lid closes | `HandleLidSwitch*` in `logind.conf` | `pmset -g` (`disablesleep`, clamshell) | `powercfg` lid action |
| Starts Aegis at boot without anyone signing in | systemd unit enabled | `launchd` daemon (not an agent) | Windows service |
| Comes back after a power cut | Firmware "AC power recovery" can't be read; shown as a manual step with how to find it | `pmset -g` `autorestart` | Manual (firmware) |
| On battery, battery health and charge | `/sys/class/power_supply/*` | `pmset -g batt`, `ioreg` | `Win32_Battery` |
| Temperature | `/sys/class/thermal`, `hwmon` | `powermetrics` (root) | WMI, best effort |
| Wi-Fi rather than Ethernet | `/sys/class/net/*/wireless` | `networksetup` | `Get-NetAdapter` |
| Disk is an SSD with space, or a USB stick | `/sys/block/*/queue/rotational`, `df` | `diskutil` | `Get-PhysicalDisk` |
| Public reachability (see H3) | outbound probe | same | same |
| Firewall state | `ufw`/`firewalld`/`nft` | `socketfilterfw` | `netsh advfirewall` |

Done when: unit tests cover parsing of each source from recorded fixtures (`logind.conf`, `pmset -g` output, `power_supply` trees), and `aegis host check --json` matches a checked-in schema.

### H2. Stay on (`aegis host prepare`) — M

Each change is shown first, asked for, backed up, recorded as an event (`HostSettingChanged { setting, old, new, actor }`) and undoable with `aegis host restore`.

1. **Never sleep while Aegis runs.** Preferred: hold a sleep inhibitor only while the daemon runs, so nothing is left behind on uninstall: `systemd-inhibit`/logind D-Bus `Inhibit("sleep:idle:handle-lid-switch")` on Linux, an IOPMAssertion (`caffeinate -s`) on macOS, `SetThreadExecutionState(ES_SYSTEM_REQUIRED)` on Windows. Optional permanent mode writes a `logind.conf.d/aegis.conf` drop-in (`HandleLidSwitch=ignore`, `HandleLidSwitchExternalPower=ignore`, `IdleAction=ignore`).
2. **Screen off, machine on.** Turn the display off after a minute (`consoleblank`, `pmset displaysleep`, `powercfg`), since a lit laptop panel wastes power and wears out.
3. **Start at boot.** The existing hardened systemd unit; a `launchd` daemon plist on macOS; a Windows service (`windows-service` crate). Apps come back up through today's daemon-restart recovery.
4. **Battery care.** Write the limit, read it back and report what stuck (many drivers accept only some values). If the laptop supports a charge limit (`charge_control_end_threshold`, ThinkPad/ASUS/Dell/Framework, or the vendor tool), offer to cap it at 80%, since a battery held at 100% on a charger swells and ages fastest. Otherwise show a warning and the vendor's setting.
5. **Wake-on-LAN** where the NIC supports it (`ethtool -s <if> wol g`), with the MAC address shown so another device (or a future Aegis node) can wake it.
6. **Automatic OS security updates** (`unattended-upgrades`/`dnf-automatic`), offered, not forced, with reboots only in a maintenance window that Aegis already knows about from schedules.

Done when: on a Linux VM in CI, `prepare` writes the logind drop-in, a sleep inhibitor is listed by `systemd-inhibit --list` while the daemon runs and gone after it stops, and `restore` puts the files back byte for byte. macOS and Windows parts start Experimental, with a manual test report in `validation/`.

### H3. Reach it from the internet (`[tunnel]`) — L

Home connections usually can't take inbound traffic: the router does NAT, many ISPs use CGNAT (no public IPv4 at all), and port 80/443 may be blocked. Aegis detects which case applies and offers the options that work, in this order:

| Mode | How | Needs | Notes |
| :--- | :--- | :--- | :--- |
| **Direct** | Router forwards 80/443 to this machine, ideally opened for you with leased mappings over PCP, then NAT-PMP, then UPnP (`crab_nat`; never UPnP exposed to the WAN); Aegis's ACME and proxy as today | A public IPv4 (not CGNAT) or IPv6 | Fastest and no third party. `host check` tells you if your WAN address is in `100.64.0.0/10` (CGNAT) or differs from what an outside echo sees. |
| **Cloudflare Tunnel** | Aegis creates the tunnel and its routes through the Cloudflare API (a scoped API token), then runs and supervises `cloudflared` like an app, mapping each Aegis domain to `http://127.0.0.1:<proxy port>` | A free Cloudflare account and a domain on Cloudflare | Works behind CGNAT; TLS ends at Cloudflare. The same tunnel on a second node becomes a replica, which gives failover for free (N5). |
| **Tailscale** | Private access through the tailnet; Funnel for a quick public link | A Tailscale account | Best for "only me and my team" dashboards. Funnel can't serve your own domain (only `*.ts.net`, ports 443/8443/10000). |
| **Aegis relay on a VPS (self-hosted)** | A tiny `aegis-relay` on any $4 VPS built on iroh (`iroh-relay`, dial by key, hole punching first); the home node dials out and holds the connection | One small VPS | No third party sees the traffic, TLS still ends at home. The same transport carries the fleet (M6) and the pool (N). |
| **Local only** | `aegis.local` over mDNS and the LAN IP | Nothing | Default for a first run. |

Supporting work:

- **Dynamic DNS** for direct mode when the home IP changes: update an A/AAAA record through Cloudflare, DuckDNS, deSEC or a generic RFC 2136 server; an event on each change (`PublicAddressChanged`).
- **ACME TLS-ALPN-01 and DNS-01** in `runtime/ingress/src/acme.rs` (today HTTP-01 only; `instant-acme` already supports the others, plus ARI renewal hints). TLS-ALPN-01 covers ISPs that block port 80 but not 443 with no DNS provider; DNS-01 (through `dns-update`) gives wildcards and works with no inbound port. Shares the provider code with Phase 7 M4's DNS integrations.
- **Address change detection** with `if-watch` (events, not polling). For IPv6, publish the address the OS reports after each change, since RFC 7217 addresses change their suffix with the prefix, or offer to set a stable token.
- **Optional own DNS on the edge node** (acme-dns pattern, `hickory-server`): delegate `home.example.com` once, then dynamic DNS and DNS-01 need no provider API.
- **Reachability probe:** after setup, request the public URL from outside (through the tunnel or a public echo the user configures) and show the result on the Host page, the same way the live URL check does today.
- **Dashboard safety:** the dashboard is never exposed through a tunnel by default; a tunnel route for it needs TOTP (Phase 7 M1) switched on first.

Done when: CI runs a direct-mode test with a fake DDNS API, a DNS-01 issuance against Pebble with a fake DNS provider, and a tunnel test where a fake `cloudflared` binary is supervised, restarted after a kill and given the right ingress config. Real Cloudflare and Tailscale stay manual tests with a written report.

### H4. Watch the hardware — M

New sampled metrics (next to today's host CPU, memory, disk and load) and alerts through the existing Slack channel and Phase 7 M3 channels:

- **On battery** (power cut or unplugged): alert at once; optionally stop low-priority apps after N minutes, and **shut down cleanly** (stop apps, checkpoint SQLite) at a set charge, so a power cut doesn't corrupt anything. Also reads a UPS through NUT (`upsc`) if present.
- **Battery health** (design vs. full capacity, cycle count, swelling risk at a constant 100%).
- **Temperature and throttling**: alert above a threshold; show it on the Host page; suggest a stand or fan.
- **Disk health** through SMART (`smartctl -j`) when installed; alert on reallocated sectors or wear.
- **Network**: link drops, Wi-Fi signal, public IP changes, tunnel up/down.

Events: `PowerSourceChanged`, `BatteryLow`, `ThermalPressure`, `DiskHealthWarning`, `TunnelStateChanged`. Prometheus gets matching gauges.

Done when: unit tests with recorded `/sys` trees and `upsc`/`smartctl -j` output; a CI test that simulates "on battery, 10%" through a fake power source and checks the clean shutdown runs.

### H5. Make the small machine go further — M

- **Low-memory profile:** a `[host] profile = "small"` that lowers default limits, keeps 1 release instead of 2, and turns on automatic storage cleanup.
- **ARM boards** (Raspberry Pi 4/5, Orange Pi): the arm64 build is cross-compiled but untested; add an arm64 CI job (GitHub's arm64 runners) and a Pi section to the install guide.
- **Scale to zero** (Phase 7 M5) matters most here: idle hobby apps release their memory and wake on the first request.
- **Turn a spare machine into a node** of another Aegis (fleet milestone): "Add this machine to my Aegis" from its own dashboard, over the relay in H3.

### H6. Docs and first run — S

- A "Turn a laptop into a server" guide: picking hardware, BIOS "restore on AC power", Ethernet over Wi-Fi, battery at 80%, keeping it cool, what a home connection can and can't do, the ISP's terms of service for hosting.
- The first-run tour on a `--home` install starts on the Host page.

### Risks for Milestone H

| Risk | Mitigation |
| :--- | :--- |
| Changing power and firewall settings on someone's personal computer | Show every change first, ask, back up, record an event, `aegis host restore` undoes all of it. Prefer a runtime inhibitor over permanent changes. |
| A lithium battery charged to 100% for months can swell | Offer a charge cap; warn when none is possible; alert on battery health. |
| Exposing a home network | Only apps you map get a public route; the dashboard is never tunnelled without TOTP; a tunnel keeps the home IP private. |
| Tunnels add a third party | The self-hosted relay mode exists for people who don't want one. |
| macOS and Windows are untested today | Linux first and validated; macOS and Windows parts ship Experimental with manual reports until CI covers them. |
| ISP terms may forbid hosting | Said plainly in the guide. |

---

## 3a. Milestone V: virtual machines from the dashboard — L

**Goal:** the machine from Milestone H also runs full VMs (Ubuntu, Debian, Fedora, Alpine; Windows with your own ISO and license). You create, start, stop, snapshot, roll back, back up and open them in the browser from the Aegis dashboard, as Proxmox does, but without reinstalling the machine. Research and options: [research_home_hosting.md §1.3 and §3](research_home_hosting.md#3-how-vms-fit-aegiss-design).

**Design:** a VM is a third workload kind next to processes and Docker containers. A `QemuLauncher` implements the existing `Launcher` trait (`runtime/process/src/lib.rs`), the way `DockerLauncher` does (`integrations/docker/src/launcher.rs`). Restart policy, logs, health checks, cgroup limits, proxy cutover, roles and per-app access then work without new code paths.

```toml
[run]
runtime = "vm"
image = "ubuntu-24.04"     # a cloud image Aegis downloads and checks against its SHA-256, or a path to a qcow2/ISO
cpus = 2
memory_mb = 2048
disk_gb = 20
ports = [8080]             # forwarded to 127.0.0.1, so domains and the proxy work as for any app
cloud_init = "cloud-init.yaml"   # optional: users, SSH keys, packages
```

| Step | What | Size |
| :--- | :--- | :--- |
| V1 | **Capability check:** `/dev/kvm` and its permissions, CPU virtualization flags (VT-x/AMD-V, and whether the firmware disabled them), QEMU installed, free disk. Shown on the Host page and in `aegis doctor`. | S |
| V2 | **Linux VMs on QEMU/KVM:** supervised `qemu-system-*` (arguments as an argv array, never a shell string), `passt` networking (QEMU ≥ 10.1) falling back to user networking, loopback port forwards, cloud-init served over HTTP through NoCloud SMBIOS (no ISO tools), an image catalog seeded from Lima's templates with SHA-256 digests, a small QMP client typed with `serde_json`, live CPU and memory limits through the existing cgroup code. | M |
| V3 | **Releases are disk snapshots:** each deploy or snapshot is a qcow2 overlay, taken with filesystems frozen through `qemu-guest-agent` when present; rollback switches to the previous overlay; retention and storage cleanup cover overlays. Events: `VmCreated`, `VmSnapshotTaken`, `VmRolledBack`, `VmDeleted`. | M |
| V4 | **Console in the browser:** QEMU VNC over WebSocket on Unix sockets only (no TCP port, no VNC password), proxied by Aegis and shown with a vendored noVNC, plus a serial console view. Admin or per-app deployer only; every console session is an audited event. | M |
| V5 | **Dashboard:** a "New VM" wizard (image, size, SSH key), VM cards with state, CPU, memory and disk, and start/stop/reboot/force-off/snapshot/rollback/delete buttons. VM backups are included in Backups. | M |
| V6 | **Bridged networking** (the VM gets its own LAN address) and attached extra disks; ISO install for Windows guests. | M |
| V7 | **Other hosts:** macOS through Apple's Virtualization framework (`vfkit`) or libkrun, with an ad-hoc signed binary (no Apple developer account), and Windows through QEMU's WHPX accelerator (works on Windows Home, keeps one `QemuLauncher`). Experimental. | L |
| V8 | **Optional backends:** drive Incus or libvirt for people who already use them, and Firecracker/Cloud Hypervisor microVMs as a fast "sandbox" runtime for apps that need stronger isolation. | L |
| V9 | **Physical to virtual:** a guided import of an old machine's disk (`virt-p2v` or Disk2vhd → `qemu-img convert` → qcow2) so a retired laptop's whole system keeps running as a VM on another node. | M |

Done when: on a CI runner with `/dev/kvm` (common on GitHub's Linux runners but not officially promised, so the test falls back to software emulation, `-accel tcg`), a test boots an Alpine cloud image, waits for its forwarded port to answer the health check, maps a domain to it through the proxy, takes a snapshot, changes a file, rolls back and sees the old file, and survives a daemon restart. A Playwright test opens the console page against a running VM.

Risks: nested virtualization on VPS hosts is often missing (V1 reports it clearly); VMs need far more memory and disk than processes, so the wizard shows what is free; a browser console is a remote shell, so it is behind roles, per-app access, TOTP when exposed through a tunnel, and the audit log.

## 3b. Milestone N: next-generation hosting — every machine you own is one pool — L (after V and the fleet)

Existing tools each solve one layer (app platforms, home-server OSes, VM managers, tunnels). This milestone joins them. See [research_home_hosting.md §4](research_home_hosting.md#4-the-next-generation-what-no-existing-tool-does).

1. **Join any machine in one command** (laptop, PC, Pi, VPS) as a node of your Aegis, built on the fleet agent (Phase 7 M6) and H1–H4's host data.
2. **Address-free networking:** nodes connect by key over iroh (QUIC, hole punching, our own relay on the edge node) and always dial out, never listen, so nothing depends on a node's IP. A node with a public IP (or a tunnel) is the public edge. Apps keep their names when a home IP changes or a laptop changes Wi-Fi.
3. **Automatic "reach" choice:** each node detects its case (public IPv4 that changes, CGNAT, IPv6 with a changing prefix; [research §2](research_home_hosting.md#2-the-three-cases-a-no-static-ip-machine-can-be-in)), picks the best working route, keeps a second one as a fallback, and switches when the first fails.
4. **Power- and health-aware placement:** when a laptop goes on battery, overheats, loses its network or its disk degrades, its workloads (processes, containers, VMs) are restarted on another node from the replicated release and volume, or Aegis wakes a sleeping one with Wake-on-LAN. Live migration only for VMs between hosts with the same CPU.
5. **Home plus edge failover:** run at home for free, fail over to a small VPS when the home connection drops (Cloudflare tunnel replicas or the iroh edge), and move back when it returns. Backups replicate across nodes.
6. **Private pools:** share spare capacity with your team or family through the existing roles and per-app access. A public marketplace (as Akash Homenode tries) is only considered after reliability and trust are proven.

Done when: three nodes in CI (one "edge" with a public address, two behind a simulated NAT) serve one app; the node running it is told it is "on battery, 10%", and the app moves to the other home node with no failed requests through the edge; the edge reports the change as events.

## 4. Milestone F: prove "always on" (S, first)

Before any of the above: a server that runs for weeks.

1. ~~**Fix the fd leak** in the supervisor.~~ Done (commit `4273822`); the nightly 60-minute soak passes.
2. **A 24 h soak** published in `validation/`, then 7 days on a real home machine (dogfooding).
3. **Run `install.sh --server` in CI** (a systemd container), including the self-update helper units.
4. **Test the macOS and Windows supervisors in CI** (start, stop, restart, logs), which H2 needs.

Done when: the 24 h report is published, and `macos-latest` and `windows-latest` run the process supervisor tests.

## 5. Milestone D: easier deploys (M–L)

What users of Coolify, Dokploy, Kamal and Dokku expect, ranked by how often it decides a choice:

1. **Docker Compose apps**: deploy a `compose.yaml` as one app, with the same health check, proxy cutover and rollback (one service is the web entry point).
2. **Buildpacks** (Nixpacks or Cloud Native Buildpacks) and **CI deploy tests for Python, Go and Rust** examples (already in `examples/`).
3. **One-click templates**: a small, reviewed catalog (Postgres, Redis, MinIO, Uptime Kuma, Plausible, n8n, Gitea, Vaultwarden) as Docker apps with volumes and backups; this is Phase 7 M5's add-ons.
4. **More Git providers**: GitLab, Gitea/Forgejo and Bitbucket webhooks next to GitHub (`git/webhook`); deploy keys for private repos.
5. **`git push aegis main`**: a built-in Git remote over SSH that deploys on push (uses `core/ssh`).
6. **Remote and multi-arch builds**: build on a stronger machine (or CI) and ship the artifact or image, so a small home server doesn't compile Rust itself.

## 6. Carried over from Phase 7, in priority order

1. **Access (M1):** per-app API tokens, then TOTP (needed by H3), then OIDC.
2. **Safer releases (M2):** deploy hooks (migrations), approval gates, preview environments per PR, canary once per-release proxy counters exist.
3. **Alerts (M3):** email, Discord, Telegram, Teams, ntfy and generic webhooks; alert rules in the dashboard. Home users need phone alerts (ntfy, Telegram) more than Slack.
4. **Backups off-site (M4):** S3-compatible upload; essential for a laptop whose disk can die.
5. **Fleet (M6):** node agent over mutual TLS, add a server by SSH, server selector; the H3 relay reuses this transport.

---

## 7. Suggested order

```
F (24 h soak, OS CI) ──► H1 check ──► H2 stay on ──► H4 hardware ──► H5
                                  │
             M1 tokens + TOTP ────┴──► H3 reach it (tunnel, DDNS, DNS-01) ──► M6 fleet / relay
                                                       │
             M4 S3 backups ──► D3 templates / add-ons ─┘
             M3 alerts (ntfy, Telegram, email) alongside H4
             D1 Compose, D2 buildpacks alongside, independent

H1 check ──► V1 KVM check ──► V2 QEMU VMs ──► V3 snapshots ──► V4 console ──► V5 dashboard ──► V6–V9
M6 fleet + H3 relay + V ──► N (one pool of machines)
```

**First slice (about 4 weeks):** F2, H1, H2 on Linux, H3 with Cloudflare Tunnel and DNS-01, and ntfy/Telegram alerts for H4's battery and heat events. At the end of it, an old Linux laptop on a home connection serves an app over HTTPS behind CGNAT, survives a lid close, a reboot and a power cut, and alerts the owner's phone.

## 8. Release mapping

| Release | Contents |
| :--- | :--- |
| **v0.9** | F, H1, H2 (Linux), M1 tokens + TOTP |
| **v0.10** | H3 (tunnel, DDNS, DNS-01), H4, M3 phone alerts, M4 S3 backups |
| **v0.11** | D1 Compose, D2 buildpacks, D3 templates, H2/H4 on macOS and Windows |
| **v0.12** | M2 hooks and previews, D4–D6, M6 fleet and the self-hosted relay, V1–V3 (Linux VMs with snapshots) |
| **v0.13** | V4–V6 (browser console, VM dashboard, bridged networking), V9 physical-to-virtual import |
| **v1.x** | N (one pool of machines with automatic reach, placement and failover), V7–V8 (macOS/Windows hosts, microVMs) |

## 9. Open questions

- Which tunnel to support first: Cloudflare (most common, needs a domain on Cloudflare) or the self-hosted relay (no third party, needs a VPS)? The plan assumes Cloudflare first.
- Should `--home` hold a sleep inhibitor only while Aegis runs (safer, the default here) or change OS settings permanently?
- Which machines matter most: old Windows laptops, Macs, or Linux mini PCs / Raspberry Pis? That decides whether macOS/Windows work moves up from v0.11.
