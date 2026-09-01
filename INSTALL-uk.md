# Посібник з встановлення ACP-A2A Gateway

**Репозиторій**: `GG-QandV/ACP-A2A_gateway`  
**Вимоги**: Rust 1.80+, git, YAML конфіг

---

## Швидкий старт (всі платформи)

```bash
# 1. Клонувати репо
git clone https://github.com/GG-QandV/ACP-A2A_gateway.git
cd ACP-A2A_gateway

# 2. Встановити Rust 1.80+ (якщо ще не встановлено)
# Дивіться інструкції для вашої платформи нижче

# 3. Зібрати
cargo build --release --workspace

# 4. Створити конфіг
cp config.example.yaml config.yaml
# Відредагувати config.yaml: токени, агенти, шляхи

# 5. Запустити
./target/release/gatewayd config.yaml
```

---

## Windows 10/11

### 1. Встановлення Rust

**Варіант A: rustup (рекомендовано)**

1. Завантажити [`rustup-init.exe`](https://win.rustup.rs/x86_64)
2. Запустити, обрати `1) Proceed with installation`
3. Після встановлення перезапустити термінал (PowerShell або CMD)
4. Перевірити: `rustc --version` → має бути `1.80+`

**Варіант B: winget (альтернатива)**

```powershell
winget install Rustlang.Rustup
```

### 2. Збірка

```powershell
# У PowerShell або CMD:
cd ACP-A2A_gateway
cargo build --release --workspace

# Бінарник: .\target\release\gatewayd.exe
```

### 3. Конфіг

Створити `config.yaml` у корені проекту:

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

**Важливо**: На Windows використовуйте подвійні зворотні слеші (`\\`) або прямі слеші (`/`) у шляхах.

### 4. Запуск

```powershell
.\target\release\gatewayd.exe config.yaml
```

### 5. Як сервіс (опціонально)

**Використовуючи NSSM (Non-Sucking Service Manager)**:

1. Завантажити [NSSM](https://nssm.cc/download)
2. `nssm install ACPGateway`
3. Application: `C:\path\to\gatewayd.exe`
4. Arguments: `config.yaml`
5. Startup directory: `C:\path\to\ACP-A2A_gateway`
6. `nssm start ACPGateway`

---

## Windows Server 2016/2019/2022

### 1. Встановлення Rust

Ті ж інструкції, що й для Windows 10/11 (rustup або winget).

**Додатково**: Встановити Visual C++ Redistributable:

```powershell
winget install Microsoft.VCRedist.2015+.x64
```

### 2. Збірка

Аналогічно Windows 10/11.

### 3. Конфіг для продакшену

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "https://gateway.example.com"  # за reverse-proxy
tokens: ["{env:GATEWAY_TOKEN}"]  # зі змінної оточення

agents:
  claurst-prod:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "C:\\workspaces\\prod"
    env:
      OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}"

task_store_dir: "D:\\gateway\\tasks"  # окремий диск для задач
task_retention_days: 30
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

### 4. Запуск як сервіс

NSSM (див. вище) або systemd через WSL2 (якщо використовується).

---

## macOS (Intel/Apple Silicon)

### 1. Встановлення Rust

**Варіант A: Homebrew (рекомендовано)**

```bash
# Встановити Homebrew, якщо ще не встановлено
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Встановити Rust
brew install rust
```

**Варіант B: rustup (альтернатива)**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. Збірка

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Бінарник: ./target/release/gatewayd
```

### 3. Конфіг

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

### 4. Запуск

```bash
./target/release/gatewayd config.yaml
```

### 5. Як сервіс (launchd)

Створити `~/Library/LaunchAgents/com.gateway.acp.plist`:

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

Завантажити сервіс:

```bash
launchctl load ~/Library/LaunchAgents/com.gateway.acp.plist
```

---

## Linux (Ubuntu/Debian/CentOS)

### 1. Встановлення Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Ubuntu/Debian**: Встановити залежності:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev
```

**CentOS/RHEL**:

```bash
sudo yum groupinstall "Development Tools"
sudo yum install -y openssl-devel
```

### 2. Збірка

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Бінарник: ./target/release/gatewayd
```

### 3. Конфіг

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

### 4. Запуск як сервіс (systemd)

Створити `/etc/systemd/system/acp-gateway.service`:

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

# Змінні оточення
Environment="GATEWAY_TOKEN=secret-token"
Environment="OPENMODEL_API_KEY=sk-..."

[Install]
WantedBy=multi-user.target
```

Запустити сервіс:

```bash
sudo systemctl daemon-reload
sudo systemctl enable acp-gateway
sudo systemctl start acp-gateway
sudo systemctl status acp-gateway
```

---

## Перевірка встановлення

```bash
# Перевірити версію Rust
rustc --version   # має бути 1.80+

# Зібрати та запустити тести
cargo test --workspace

# Запустити шлюз
./target/release/gatewayd config.yaml

# Перевірити картку агента
curl -H "Authorization: Bearer t-dev-local-001" \
     http://localhost:8348/agents/claurst-main/.well-known/agent.json
```

---

## Приклад конфігу (config.example.yaml)

```yaml
# config.example.yaml
listen: "0.0.0.0:8347"          # TCP для ACP клієнтів
http_listen: "0.0.0.0:8348"     # HTTP для A2A клієнтів
public_url: "http://localhost:8348"  # зовнішня адреса (за reverse-proxy)

tokens:
  - "t-dev-local-001"           # токени клієнтів

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

## Наступні кроки

1. Налаштувати reverse-proxy (nginx, Caddy, Traefik) для TLS
2. Додати токени у `tokens: []`
3. Налаштувати агентів у `agents: {}`
4. Запустити шлюз: `./target/release/gatewayd config.yaml`
5. Перевірити картку агента: `curl -H "Authorization: Bearer <token>" http://localhost:8348/agents/<id>/.well-known/agent.json`

---

## Вирішення проблем

### Застаріла версія Rust

```bash
rustup update
rustc --version   # перевірити 1.80+
```

### Помилка збірки на Windows

Встановити Visual C++ Build Tools:
```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

### Помилка збірки на Linux (відсутній OpenSSL)

```bash
# Ubuntu/Debian
sudo apt install -y libssl-dev

# CentOS/RHEL
sudo yum install -y openssl-devel
```

### Відмова в доступі на macOS/Linux

```bash
chmod +x ./target/release/gatewayd
```

### Порт вже використовується

Змінити `listen` та `http_listen` у config.yaml:
```yaml
listen: "0.0.0.0:8349"
http_listen: "0.0.0.0:8350"
```

---

## Підтримка

- Документація: папка `docs/` у репозиторії
- Проблеми: GitHub Issues
- Специфікації протоколів: [ACP](https://agentclientprotocol.com), [A2A](https://google.github.io/A2A/)