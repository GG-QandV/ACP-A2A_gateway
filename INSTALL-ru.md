# Установка ACP-A2A Gateway


> **Язык:** русский · [English version](./INSTALL.md)
**Репозиторий**: `GG-QandV/ACP-A2A_gateway`  
**Требования**: Rust 1.80+, git, C-компилятор, YAML-конфиг

---

## Какой способ установки выбрать

| Способ | Проверено? | Кому подходит |
|---|---|---|
| Клон + `cargo build --release` | **да — Linux x86_64 на v1.1.2.** Сборки под Windows и macOS описаны, но нами не тестировались | любая ОС; единственный способ получить `gatewayd.exe` или бинарник под Apple Silicon |
| Готовый бинарник из Releases | только Linux x86_64 | вы на x86-64 Linux и не хотите поднимать Rust-тулчейн |
| WSL2 + линуксовый бинарник | нами не тестировалось | вы на Windows 10/11 и хотите вообще без нативной сборки |

Два ограничения, из которых растёт эта таблица:

- **C-тулчейн нужен всегда.** `rusqlite` подключён с фичей `features = ["bundled"]`
  (`gatewayd/Cargo.toml:40`), то есть сам SQLite компилируется из исходников: MSVC
  «Desktop development with C++» на Windows, Xcode Command Line Tools на macOS,
  `gcc` + `libssl-dev` на Linux.
- **Бинарник работает только в той ОС, под которую собран.** Линуксовый файл — это ELF
  (`interpreter /lib64/ld-linux-x86-64.so.2`); ядро Windows и macOS его не загрузит, и
  Rosetta не помогает — она транслирует инструкции, а не формат файла. TLS берётся из
  `native-tls`: на Linux бинарник в рантайме линкуется с OpenSSL 3, а на Windows это
  schannel и Security.framework на macOS — там OpenSSL не нужен.

### Windows 10/11

- **Рекомендуем:** собрать нативно в `target\release\gatewayd.exe`
  (см. [Windows 10/11](#windows-1011)). Нами не проверено — заложите время на установку
  Build Tools (`winget install Microsoft.VisualStudio.2022.BuildTools`, workload
  «Desktop development with C++»).
- **Без тулчейна:** запускить линуксовый бинарник внутри **WSL2**
  (`wsl --install -d Ubuntu-24.04`, затем `sudo apt install libssl3`). Нами не тестировалось,
  и есть три грабли: (1) пути по умолчанию в unix-стиле `/tmp/gateway/...`, а `/tmp` при
  выключении WSL очищается — направьте `task_store_dir` и `.db`-пути журнала, event log и
  approvals в постоянный каталог; (2) агенты запускаются внутри WSL, значит `claurst` /
  `opencode` должны быть установлены там же; (3) `localhost` работает из коробки, а вот доступ
  из локалки требует правила `netsh portproxy` и разрешения в фаерволе.

### macOS

Собирать на самой машине (`cargo build --release`): получится бинарник под **вашу**
архитектуру (Apple Silicon arm64 или Intel x86_64), а darwin-ассет не публикуется — то есть
это единственный реалистичный путь. Шаги в разделе
[macOS (Intel/Apple Silicon)](#macos-intelapple-silicon), описаны, но нами не тестировались.
Локально собранный бинарник запускается сразу; для неподписанного, скачанного откуда-то, нужно
сначала `xattr -d com.apple.quarantine ./gatewayd`.

### Linux x86-64 — проверенный путь

Либо собрать (`cargo build --release --workspace`), либо взять ассет `gatewayd` из Releases,
`chmod +x` и запускать. Кроме glibc нужно только OpenSSL 3 (`libssl3`) — он есть в
Debian 12 / Ubuntu 22.04 и новее.

---

## Быстрый старт (все платформы)

```bash
# 1. Клонировать репо
git clone https://github.com/GG-QandV/ACP-A2A_gateway.git
cd ACP-A2A_gateway

# 2. Установить Rust 1.80+ (если нет)
# См. инструкции ниже для вашей платформы

# 3. Собрать
cargo build --release --workspace

# 4. Создать конфиг
cp config.example.yaml config.yaml
# Отредактировать config.yaml: токены, агенты, пути

# 5. Запустить
./target/release/gatewayd config.yaml
```

---

## Windows 10/11

### 1. Установка Rust

**Способ A: rustup (рекомендуется)**

1. Скачать [`rustup-init.exe`](https://win.rustup.rs/x86_64)
2. Запустить, выбрать `1) Proceed with installation`
3. После установки перезапустить терминал (PowerShell или CMD)
4. Проверить: `rustc --version` → должно быть `1.80+`

**Способ B: winget (альтернатива)**

```powershell
winget install Rustlang.Rustup
```

### 2. Сборка

```powershell
# В PowerShell или CMD:
cd ACP-A2A_gateway
cargo build --release --workspace

# Бинарник: .\target\release\gatewayd.exe
```

### 3. Конфиг

Создать `config.yaml` в корне проекта:

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

**Важно**: пути в Windows указывать с двойными слешами (`\\`) или прямыми (`/`).

### 4. Запуск

```powershell
.\target\release\gatewayd.exe config.yaml
```

### 5. Как сервис (опционально)

** NSSM (Non-Sucking Service Manager)**:

1. Скачать [NSSM](https://nssm.cc/download)
2. `nssm install ACPGateway`
3. Application: `C:\path\to\gatewayd.exe`
4. Arguments: `config.yaml`
5. Startup directory: `C:\path\to\ACP-A2A_gateway`
6. `nssm start ACPGateway`

---

## Windows Server 2016/2019/2022

### 1. Установка Rust

Те же инструкции, что для Windows 10/11 (rustup или winget).

**Дополнительно**: установить Visual C++ Redistributable:

```powershell
winget install Microsoft.VCRedist.2015+.x64
```

### 2. Сборка

Аналогично Windows 10/11.

### 3. Конфиг для прода

```yaml
listen: "0.0.0.0:8347"
http_listen: "0.0.0.0:8348"
public_url: "https://gateway.example.com"  # за reverse-proxy
tokens: ["{env:GATEWAY_TOKEN}"]  # из переменной окружения

agents:
  claurst-prod:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "C:\\workspaces\\prod"
    env:
      OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}"

task_store_dir: "D:\\gateway\\tasks"  # отдельный диск для задач
task_retention_days: 30
turn_lease_timeout_secs: 30
agent_call_timeout_secs: 120
```

### 4. Запуск как сервис

NSSM (см. выше) или systemd через WSL2 (если используется).

---

## macOS (Intel/Apple Silicon)

### 1. Установка Rust

**Способ A: Homebrew (рекомендуется)**

```bash
# Установить Homebrew, если нет
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Установить Rust
brew install rust
```

**Способ B: rustup (альтернатива)**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. Сборка

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Бинарник: ./target/release/gatewayd
```

### 3. Конфиг

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

### 5. Как сервис (launchd)

Создать `~/Library/LaunchAgents/com.gateway.acp.plist`:

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

Загрузить:

```bash
launchctl load ~/Library/LaunchAgents/com.gateway.acp.plist
```

---

## Linux (Ubuntu/Debian/CentOS)

### 1. Установка Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Ubuntu/Debian**: установить зависимости:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev
```

**CentOS/RHEL**:

```bash
sudo yum groupinstall "Development Tools"
sudo yum install -y openssl-devel
```

### 2. Сборка

```bash
cd ACP-A2A_gateway
cargo build --release --workspace

# Бинарник: ./target/release/gatewayd
```

### 3. Конфиг

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

### 4. Запуск как сервис (systemd)

Создать `/etc/systemd/system/acp-gateway.service`:

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

# Переменные окружения
Environment="GATEWAY_TOKEN=secret-token"
Environment="OPENMODEL_API_KEY=sk-..."

[Install]
WantedBy=multi-user.target
```

Запустить:

```bash
sudo systemctl daemon-reload
sudo systemctl enable acp-gateway
sudo systemctl start acp-gateway
sudo systemctl status acp-gateway
```

---

## Проверка установки

```bash
# Проверить версию Rust
rustc --version   # должно быть 1.80+

# Собрать и запустить тесты
cargo test --workspace

# Запустить шлюз
./target/release/gatewayd config.yaml

# Проверить карточку агента
curl -H "Authorization: Bearer t-dev-local-001" \
     http://localhost:8348/agents/claurst-main/.well-known/agent.json
```

---

## Пример конфига (config.example.yaml)

```yaml
# config.example.yaml
listen: "0.0.0.0:8347"          # TCP для ACP-клиентов
http_listen: "0.0.0.0:8348"     # HTTP для A2A-клиентов
public_url: "http://localhost:8348"  # внешний адрес (за reverse-proxy)

tokens:
  - "t-dev-local-001"           # токены клиентов

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

## Следующие шаги

1. Настроить reverse-proxy (nginx, Caddy, Traefik) для TLS
2. Добавить токены в `tokens: []`
3. Настроить агентов в `agents: {}`
4. Запустить шлюз: `./target/release/gatewayd config.yaml`
5. Проверить карточку агента: `curl -H "Authorization: Bearer <token>" http://localhost:8348/agents/<id>/.well-known/agent.json`