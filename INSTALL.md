# ACP-A2A Gateway Installation Guide

**Repository**: `GG-QandV/ACP-A2A_gateway`  
**Requirements**: Rust 1.80+, git, a C compiler, YAML config

---

## Which install path to use

| Path | Verified? | Use it if |
|---|---|---|
| Clone + `cargo build --release` | **Linux x86_64: verified** (built + 152 tests at v1.1.2). Windows/macOS source builds compile in CI, but we never run them | any OS; the fallback if an asset misbehaves |
| Prebuilt binary from Releases | **compile-verified by CI** for linux-x86_64, windows-x86_64, darwin-arm64, darwin-x86_64 (+ universal); runtime on Windows/macOS not exercised by us | you just want to run the gateway |
| WSL2 + the Linux binary | not tested by us | you are on Windows 10/11 and want zero native setup |

Two constraints shape this table:

- **A C toolchain is always required.** `rusqlite` is pulled in with `features = ["bundled"]`
  (`gatewayd/Cargo.toml:40`), so SQLite itself is compiled from source: MSVC
  "Desktop development with C++" on Windows, Xcode Command Line Tools on macOS,
  `gcc` + `libssl-dev` on Linux.
- **A binary runs only on the OS it was built for.** The Linux file is ELF
  (`interpreter /lib64/ld-linux-x86-64.so.2`); Windows and macOS kernels will not load it,
  and Rosetta does not help — it translates instructions, not file formats. TLS comes from
  `native-tls`: Linux builds link OpenSSL 3 at runtime, while Windows uses schannel and
  macOS uses Security.framework, so neither of those needs OpenSSL.

### Windows 10/11

- **Fastest:** download `gatewayd-v<version>-windows-x86_64.exe` from Releases. It is built by
  our release workflow on a Windows runner, so the *compile* is verified; we do not run Windows
  ourselves, so treat the first start as a smoke test. TLS is schannel — no OpenSSL to install.
  Remove the download mark first: `Unblock-File .\gatewayd-v<version>-windows-x86_64.exe`.
- **Build it yourself:** `cargo build --release` → `target\release\gatewayd.exe`
  (see [Windows 10/11](#windows-1011)). Needs the MSVC C++ workload, because SQLite is compiled
  from source: `winget install Microsoft.VisualStudio.2022.BuildTools`.
- **Without a toolchain, inside WSL2:** take the Linux binary
  (`wsl --install -d Ubuntu-24.04`, then `sudo apt install libssl3`). Untested by us, and
  three things bite: (1) the default paths are Unix-style `/tmp/gateway/...`, and `/tmp` is
  wiped when WSL shuts down — point `task_store_dir` and the journal / event-log / approvals
  `.db` paths at a persistent directory; (2) agents are spawned inside WSL, so
  `claurst` / `opencode` must be installed there too; (3) `localhost` works out of the box,
  but reaching the gateway from your LAN needs a `netsh portproxy` rule plus a firewall allow.

### macOS

Pick the asset for your machine: `darwin-arm64` (Apple Silicon), `darwin-x86_64` (Intel), or
`darwin-universal` (both). Like the Windows build, these are CI-compiled but not CI-run.
Binaries are unsigned, so a downloaded copy needs the quarantine flag cleared before the first
start: `xattr -d com.apple.quarantine ./gatewayd-v<version>-darwin-arm64`. Building on the
machine (`cargo build --release`) is the alternative and automatically matches your
architecture — steps in [macOS (Intel/Apple Silicon)](#macos-intelapple-silicon).

### Linux x86-64 — the verified path

Either build (`cargo build --release --workspace`) or take the `gatewayd` asset from Releases,
`chmod +x` it and run. The only runtime requirement beyond glibc is OpenSSL 3 (`libssl3`),
present on Debian 12 / Ubuntu 22.04 and newer.

---

## Quick Start (All Platforms)

```bash
# 1. Clone the repo
git clone https://github.com/GG-QandV/ACP-A2A_gateway.git
cd ACP-A2A_gateway

# 2. Install Rust 1.80+ (if not already installed)
# See platform-specific instructions below

# 3. Build
cargo build --release --workspace

# 4. Create config
cp config.example.yaml config.yaml
# Edit config.yaml: tokens, agents, paths

# 5. Run
./target/release/gatewayd config.yaml
```

---

## Windows 10/11

### 1. Install Rust

**Option A: rustup (recommended)**

1. Download [`rustup-init.exe`](https://win.rustup.rs/x86_64)
2. Run it, select `1) Proceed with installation`
3. After installation, restart your terminal (PowerShell or CMD)
4. Verify: `rustc --version` → should show `1.80+`

**Option B: winget (alternative)**

```powershell
winget install Rustlang.Rustup
```

### 2. Build

```powershell
# In PowerShell or CMD:
cd ACP-A2A_gateway
cargo build --release --workspace

# Binary: .\target\release\gatewayd.exe
```

### 3. Config

Create `config.yaml` in the project root:

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "http://localhost:8348"
tokens: ["t-dev-local-001"]

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "C:\\path\\to\\workspace"
    env: {}

task_store_dir: "C:\\gateway\\tasks"
task_retention_days: 7
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

**Important**: On Windows, use double backslashes (`\\`) or forward slashes (`/`) in paths.

### 4. Run

```powershell
.\target\release\gatewayd.exe config.yaml
```

### 5. As a Service (Optional)

**Using NSSM (Non-Sucking Service Manager)**:

1. Download [NSSM](https://nssm.cc/download)
2. `nssm install ACPGateway`
3. Application: `C:\path\to\gatewayd.exe`
4. Arguments: `config.yaml`
5. Startup directory: `C:\path\to\ACP-A2A_gateway`
6. `nssm start ACPGateway`

---

## Windows Server 2016/2019/2022

### 1. Install Rust

Same instructions as Windows 10/11 (rustup or winget).

**Additionally**: Install Visual C++ Redistributable:

```powershell
winget install Microsoft.VCRedist.2015+.x64
```

### 2. Build

Same as Windows 10/11.

### 3. Production Config

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "https://gateway.example.com"  # behind reverse-proxy
tokens: ["{env:GATEWAY_TOKEN}"]  # from environment variable

agents:
  claurst-prod:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "C:\\workspaces\\prod"
    env:
      OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}"

task_store_dir: "D:\\gateway\\tasks"  # separate disk for tasks
task_retention_days: 30
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

### 4. Run as Service

NSSM (see above) or systemd via WSL2 (if using).

---

## macOS (Intel/Apple Silicon)

### 1. Install Rust

**Option A: Homebrew (recommended)**

```bash
# Install Homebrew if not already installed
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install Rust
brew install rust
```

**Option B: rustup (alternative)**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. Build

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Binary: ./target/release/gatewayd
```

### 3. Config

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "https://gateway.example.com"
tokens: ["t-dev-local-001"]

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/Users/username/workspace"
    env: {}

task_store_dir: "/Users/username/gateway/tasks"
task_retention_days: 7
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

### 4. Run

```bash
./target/release/gatewayd config.yaml
```

### 5. As a Service (launchd)

Create `~/Library/LaunchAgents/com.gateway.acp.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.gateway.acp</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/username/ACP-A2A_gateway/target/release/gatewayd</string>
        <string>/Users/username/ACP-A2A_gateway/config.yaml</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/Users/username/ACP-A2A_gateway</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Users/username/gateway.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/username/gateway.err</string>
</dict>
</plist>
```

Load the service:

```bash
launchctl load ~/Library/LaunchAgents/com.gateway.acp.plist
```

---

## Linux (Ubuntu/Debian/CentOS)

### 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Ubuntu/Debian**: Install dependencies:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev
```

**CentOS/RHEL**:

```bash
sudo yum groupinstall "Development Tools"
sudo yum install -y openssl-devel
```

### 2. Build

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Binary: ./target/release/gatewayd
```

### 3. Config

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "https://gateway.example.com"
tokens: ["{env:GATEWAY_TOKEN}"]

agents:
  claurst-prod:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/srv/workspaces/prod"
    env:
      OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}"

task_store_dir: "/srv/gateway/tasks"
task_retention_days: 30
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

### 4. Run as Service (systemd)

Create `/etc/systemd/system/acp-gateway.service`:

```ini
[Unit]
Description=ACP-A2A Gateway
After=network.target

[Service]
Type=simple
User=gateway
Group=gateway
WorkingDirectory=/srv/ACP-A2A_gateway
ExecStart=/srv/ACP-A2A_gateway/target/release/gatewayd /srv/ACP-A2A_gateway/config.yaml
Restart=on-failure
RestartSec=5s

# Environment variables
Environment="GATEWAY_TOKEN=secret-token"
Environment="OPENMODEL_API_KEY=sk-..."

[Install]
WantedBy=multi-user.target
```

Start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable acp-gateway
sudo systemctl start acp-gateway
sudo systemctl status acp-gateway
```

---

## Verify Installation

```bash
# Check Rust version
rustc --version   # should be 1.80+

# Build and run tests
cargo test --workspace

# Run the gateway
./target/release/gatewayd config.yaml

# Check agent card
curl -H "Authorization: Bearer t-dev-local-001" \
     http://localhost:8348/agents/claurst-main/.well-known/agent.json
```

---

## Example Config (config.example.yaml)

```yaml
# config.example.yaml
listen: "0.0.0.0:8347"          # TCP for ACP clients
http_listen: "0.0.0.0:8348"     # HTTP for A2A clients
public_url: "http://localhost:8348"  # external URL (behind reverse-proxy)

tokens:
  - "t-dev-local-001"           # client tokens

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/path/to/workspace"
    env: {}

  hermes-main:
    transport: stdio
    command: ["hermes", "acp"]
    cwd: "/tmp"

  ops-agent:
    transport: http
    url: "https://ops.internal/a2a"
    push_token: "{env:OPS_AGENT_PUSH_TOKEN}"

task_store_dir: "/tmp/gateway/tasks"
task_retention_days: 7
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

---

## Next Steps

1. Configure reverse-proxy (nginx, Caddy, Traefik) for TLS
2. Add tokens to `tokens: []`
3. Configure agents in `agents: {}`
4. Run the gateway: `./target/release/gatewayd config.yaml`
5. Verify agent card: `curl -H "Authorization: Bearer <token>" http://localhost:8348/agents/<id>/.well-known/agent.json`

---

## Troubleshooting

### Rust version too old

```bash
rustup update
rustc --version   # verify 1.80+
```

### Build fails on Windows

Install Visual C++ Build Tools:
```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

### Build fails on Linux (missing OpenSSL)

```bash
# Ubuntu/Debian
sudo apt install -y libssl-dev

# CentOS/RHEL
sudo yum install -y openssl-devel
```

### Permission denied on macOS/Linux

```bash
chmod +x ./target/release/gatewayd
```

### Port already in use

Change `listen` and `http_listen` in config.yaml:
```yaml
listen: "0.0.0.0:8349"
http_listen: "0.0.0.0:8350"
```

---

## Support

- Documentation: `docs/` folder in the repository
- Issues: GitHub Issues
- Protocol specs: [ACP](https://agentclientprotocol.com), [A2A](https://google.github.io/A2A/)