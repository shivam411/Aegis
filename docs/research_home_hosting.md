# Research: turning ordinary computers into servers, VMs and dynamic IPs

Date: 2026-11-04. Input to [plan_phase8.md](plan_phase8.md). It answers three questions:

1. Can a normal laptop or PC become a server managed by Aegis?
2. Can that machine run full virtual machines (VMs) from the Aegis dashboard?
3. Can it work without a static IP?

**Short answers:** yes, yes, and yes. Each piece already exists somewhere as a separate tool. No single product puts them together for ordinary hardware with Aegis's one-binary, safe-deploy approach. That gap is the opportunity.

Sources are listed at the end. Most are third-party blogs and forums, so version numbers and counts are approximate. Points marked *(background)* come from general knowledge rather than this search.

---

## 1. What already exists

### 1.1 App platforms you host yourself (all assume a VPS with a public IP)

| Tool | Strength | Gap for home hardware |
| :--- | :--- | :--- |
| Coolify | Hundreds of one-click services, Compose, many Git providers, multi-server | Needs a reachable server. Had 11 vulnerabilities disclosed in Jan 2026, 3 rated CVSS 10.0. |
| Dokploy | Lighter, Docker/Swarm, Nixpacks, databases | Same reachability assumption. The license has restrictions on top of Apache 2.0. |
| Kamal 2 | `kamal-proxy` switches traffic only after the health check passes; remote builders; "accessories" (databases, caches) | CLI only; no power or network awareness. |
| CapRover, Dokku | Simple, Heroku-like | Same. |

### 1.2 "Home server" operating systems and app stores

| Tool | Strength | Gap |
| :--- | :--- | :--- |
| Umbrel | Most polished for non-technical users | Not fully open source (free for personal use); app-store focus, not app deploys from Git. |
| CasaOS / ZimaOS | Beginner-friendly Docker dashboard | CasaOS development appears to have slowed; IceWhale is moving to ZimaOS. |
| Runtipi | Many Docker apps on one box, one-click installs, backups | App store, not a deploy pipeline. |
| YunoHost | Largest catalog; mail, calendar, contacts; Debian based | Takes over the whole OS. |

None of these handles a laptop's lid, battery or heat, or deploys your own code with health checks and rollback.

### 1.3 Virtual machine managers

| Tool | How it works | Notes |
| :--- | :--- | :--- |
| Proxmox VE | Its own OS; QEMU/KVM VMs plus LXC containers; mature web UI; REST API; clustering | Easiest to start with, but you reinstall the machine to use it. |
| Incus (the LXD fork) | Runs on any Linux; one REST API and CLI for containers and VMs; clustering | Its web UI is a separate, less polished package. |
| Cockpit + `cockpit-machines` | Web console over libvirt; works with existing `virsh` VMs; VNC console | Basic: some tasks still need `virt-manager` or the CLI. |
| Firecracker | Rust microVM manager on KVM (AWS); very fast boot; dense isolation | KVM only, no GPU passthrough; needs your own orchestration. |
| Cloud Hypervisor | Rust VMM (Linux Foundation); GPU passthrough via VFIO; KVM and Microsoft Hypervisor | No built-in rate limiting. |
| Kata Containers | Containers that run inside a VM (QEMU, Firecracker or Cloud Hypervisor) | Kubernetes-oriented. |
| macOS: Apple Virtualization framework (`vfkit`, Lima, `vz` bindings) | Linux VMs on Macs without third-party kernel extensions | The calling binary needs the `com.apple.security.virtualization` entitlement, so it must be signed. |
| Windows: Hyper-V, WSL2 *(background)* | Built into Windows Pro/Server; scriptable from PowerShell | Not available on Windows Home. |

### 1.4 Turning an existing computer *into* a VM (physical to virtual) *(background)*

| Tool | What it does |
| :--- | :--- |
| `virt-p2v` / `virt-v2v` (libguestfs) | Boots the physical machine from a small image and streams its disk to a KVM host as a VM. |
| Sysinternals Disk2vhd | Captures a running Windows machine to a VHDX file that Hyper-V or QEMU can boot. |
| Clonezilla, `dd` + `qemu-img convert` | Raw disk image converted to qcow2. |

### 1.5 Reaching a machine without a static IP

| Tool | Model | Notes |
| :--- | :--- | :--- |
| Cloudflare Tunnel (`cloudflared`) | Dials out to Cloudflare's network and holds the connection; no inbound port | Works behind CGNAT; free tier with DDoS protection; HTTP(S) mainly; Cloudflare terminates TLS. |
| Tailscale (and Funnel) | WireGuard mesh; direct when NAT hole punching works, relayed otherwise | Great for private access. Public hosting needs Funnel. |
| Pangolin | Self-hosted: a VPS runs Traefik, Pangolin (authentication) and Gerbil (WireGuard); a "Newt" connector at home dials out | No third party sees your traffic. Needs a VPS with 80/443 TCP and two UDP ports open; cloud firewalls that drop UDP break it. |
| frp, rathole, zrok, ngrok, playit.gg *(background)* | Reverse tunnels to a relay | Various open/closed models. |
| Dynamic DNS (DuckDNS, deSEC, Cloudflare API) | Update a DNS record when the home IP changes | Only works if you have a public IP. |
| IPv6 | Many ISPs give a public IPv6 prefix, but it can change on a router reboot (one ISP gives a /56 with a 26-minute lease) | Fix: a stable host suffix plus a DNS update with the new prefix on every change. Privacy addresses make it harder to know which address to publish. |

### 1.6 Sharing home compute with others

- **Akash Homenode** (open beta since Feb 2026) lets people rent out home GPUs. Akash says prices are 50–80% below the big clouds, with fewer guarantees. Its live provider count was about 60 in July 2026, so this is early.
- **Flux** runs Docker apps on user-operated nodes. Its claims are mostly its own marketing.

This shows there is interest in pooled home compute. It also shows the hard parts: reliability, bandwidth and trust.

---

## 2. The three cases a "no static IP" machine can be in

| Case | How to tell | What works |
| :--- | :--- | :--- |
| **A. Public IPv4 that changes** | The router's WAN address equals what an outside echo sees, and is not in `100.64.0.0/10` | Port forward (UPnP-IGD / NAT-PMP / PCP) + dynamic DNS + ACME. Fastest, no third party. |
| **B. Behind CGNAT or a blocked port 80/443** | WAN address in `100.64.0.0/10`, or it differs from the outside echo, or an inbound probe fails | An outbound tunnel: Cloudflare Tunnel, Tailscale Funnel, or a self-hosted relay (Pangolin-style) on a cheap VPS. |
| **C. Public IPv6 with a changing prefix** | A global IPv6 address exists and an inbound IPv6 probe succeeds | Publish AAAA with a stable suffix and update it when the prefix changes; open the host firewall for 443 only. Visitors who only have IPv4 still need A or B. |

Aegis can detect the case itself and pick the best working option. It can keep a second option as a fallback (for example, direct IPv6 plus a tunnel for IPv4) and switch when one fails. To visitors the app always has the same name, whatever the IP does.

---

## 3. How VMs fit Aegis's design

Aegis already runs two kinds of workloads under the same supervisor through a small `Launcher` abstraction (`runtime/process`): plain processes and Docker containers. Restart policy, logs, health checks, proxy cutover, rollback and cgroup limits are shared. A VM fits as a third launcher:

- **Linux:** run `qemu-system-*` with KVM directly as a supervised process, as the Docker runtime drives the `docker` CLI. No libvirt is needed, and cgroup limits apply to the QEMU process as they do today. An optional backend can drive Incus or libvirt for people who already use them.
- **Releases map to disk snapshots:** a VM's "release" is a qcow2 overlay on a base image, so rollback switches back to the previous overlay without rebuilding, which matches immutable releases ([ADR 0007](adr/0007-immutable-releases.md)).
- **Network:** user-mode networking with port forwards to `127.0.0.1` for a start (no root, like Docker's loopback-only ports), so the built-in proxy, domains and health checks work unchanged. Bridged or tap networking comes later for "a VM with its own LAN address".
- **First boot:** standard cloud images (Ubuntu, Debian, Fedora, Alpine) with a cloud-init seed (SSH key, user, packages). Optional Windows guests use the user's own ISO and license.
- **Console in the browser:** QEMU's VNC with WebSocket enabled, shown through noVNC in the dashboard, and the serial console in a terminal view, all behind the existing auth, roles and per-app access.
- **macOS and Windows hosts:** Apple's Virtualization framework (`vfkit`; needs a signed binary with the entitlement) and Hyper-V. These come later and start Experimental.
- **Fast microVMs:** Firecracker or Cloud Hypervisor as an optional "sandbox" runtime for apps that need stronger isolation than a process or container. Both are written in Rust, as Aegis is.

"Convert a laptop into a VM" can mean two things, and both are covered:

1. **The laptop becomes a VM host** (the main path): Aegis on the laptop creates, starts, stops, snapshots and backs up VMs from the dashboard, as Proxmox does, but without reinstalling the machine.
2. **The laptop's existing system becomes a VM** (physical to virtual, later): capture its disk with `virt-p2v` or Disk2vhd, convert it to qcow2, and run it as a VM on another Aegis node. This is useful for retiring old hardware without losing its setup.

---

## 4. The next generation: what no existing tool does

Existing tools each solve one layer. The next step is to treat **every machine you own as one pool**, whatever its network:

1. **Any machine joins in one command:** laptop, desktop, Raspberry Pi, old server or VPS. Each becomes a node, with its power, battery, heat, disk and network known to the controller.
2. **One workload model:** process, container, VM or microVM. Each gets the same health-checked deploy, rollback, limits, logs and backups.
3. **Addresses stop mattering:** nodes talk over an encrypted WireGuard mesh. The public edge is a node that does have a public IP (a cheap VPS) or a tunnel. Apps keep their names when a home IP changes or a laptop moves to another Wi-Fi.
4. **Power- and health-aware placement:** when a laptop goes on battery, overheats or its disk degrades, Aegis moves its workloads to another node (or wakes one with Wake-on-LAN) before anything fails.
5. **Home plus edge failover:** the app runs at home for free and fails over to a small VPS (or the reverse) when the home connection drops. Backups replicate across nodes.
6. **Private pools first:** share spare capacity with your own team or family through per-app access control. A public marketplace (like Akash Homenode) is a possible later step, but only after reliability and trust are proven.

This builds on what Aegis already has: the supervisor and launchers, the proxy and zero-downtime cutover, the event-sourced audit trail, roles and per-app access, backups and the planned fleet agent.

---

## Sources

- [Dokploy vs Coolify 2026 (INTROSERV)](https://introserv.com/blog/dokploy-vs-coolify-complete-comparison-of-the-best-self-hosted-paas-platforms-for-vps-and-dedicated-servers-2026/)
- [Coolify vs Dokploy (dev.to)](https://dev.to/jangwook_kim_e31e7291ad98/coolify-vs-dokploy-best-self-hosted-paas-for-developers-2026-oeo)
- [Self-Hosted PaaS Showdown 2026 (Deploynix)](https://deploynix.io/blog/self-hosted-paas-showdown-2026-coolify-vs-dokploy-vs-caprover-vs-deploynix)
- [Rails 8 and Kamal 2 (Saeloun)](https://blog.saeloun.com/2026/07/23/rails-8-kamal-2-deployment-zero-downtime/)
- [Proxmox VE vs Incus (homelab-guide)](https://homelab-guide.pages.dev/proxmox-vs-incus-comparison)
- [Incus as a Proxmox alternative (gyptazy)](https://gyptazy.com/blog/incus-for-containers-and-vms-a-powerful-proxmox-alternative-a-step-by-step-guide-to-build-a-cluster/)
- [Cockpit: virtual machines](https://cockpit-project.org/guide/195/feature-virtualmachines.html)
- [RHEL: managing VMs in the web console](https://docs.redhat.com/en/documentation/Red_Hat_Enterprise_Linux/9/html/configuring_and_managing_virtualization/managing-virtual-machines-in-the-web-console_configuring-and-managing-virtualization)
- [Firecracker vs Cloud Hypervisor (Northflank)](https://northflank.com/blog/firecracker-vs-cloud-hypervisor)
- [Firecracker (Fly.io)](https://fly.io/learn/firecracker-vm.md)
- [NQRust-MicroVM](https://awesome.ecosyste.ms/projects/github.com%2Fnexusquantum%2Fnqrust-microvm)
- [vfkit Homebrew formula](https://github.com/cfergeau/homebrew-crc/blob/main/vfkit.rb), [Code-Hex/vz](https://github.com/Code-Hex/vz), [gpu-cli/vz](https://awesome.ecosyste.ms/projects/github.com%2Fgpu-cli%2Fvz)
- [CasaOS alternatives (AlternativeTo)](https://alternativeto.net/software/casaos), [CasaOS vs YunoHost (OpenAlternative)](https://openalternative.co/compare/casaos/vs/yunohost), [Umbrel alternatives (OpenAlternative)](https://openalternative.co/alternatives/umbrel)
- [Pangolin (GitHub)](https://github.com/hhftechnology/pangolin), [Install Pangolin (ComputingForGeeks)](https://computingforgeeks.com/install-pangolin-self-hosted-reverse-proxy/), [Pangolin as a Cloudflare Tunnel alternative (Bitdoze)](https://www.bitdoze.com/pangolin-cloudflare-tunnels-alternative/)
- [CGNAT: four ways to reach a service (dev.to)](https://dev.to/pstayet/cgnat-broke-my-server-four-ways-to-reach-a-service-behind-carrier-grade-nat-4852), [Mac mini through a Cloudflare Tunnel (dev.to)](https://dev.to/vineethnkrishnan/pointing-vinelabsde-at-my-mac-mini-through-a-cloudflare-tunnel-5h85), [Ubuntu laptop home server with Tailscale (dev.to)](https://dev.to/reggiejohnson86/-ubuntu-laptop-home-server-personal-vpn-tailscale-exit-node-18b)
- [Self-host at home with a changing IPv6 prefix (ounapuu.ee)](https://ounapuu.ee/posts/2026/05/06/self-host-at-home/), [Dynamic IPv4, IPv6 prefix and WireGuard (engineered.at)](https://engineered.at/articles/how-i-self-host-this-blog-at-home-with-a-dynamic-ipv4-address-ipv6-prefix-and-a-dash-of-wireguard), [RFC 3633](https://www.rfc-editor.org/rfc/inline-errata/rfc3633.html)
- [Akash Homenode early access (PR Newswire)](https://tools.prnewswire.com/en-us/live/20813/release/20260225EN94703), [Akash Q1 2026 report](https://akash.network/blog/akash-network-q1-2026-report/), [Flux: state of compute networks 2026](https://www.runonflux.com/flux-against-the-world-state-of-compute-networks-2026/)
