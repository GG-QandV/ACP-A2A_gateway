---
name: gateway-client
description: >-
  Як агенту (Claude Code, Gemini CLI, Codex, Cline, opencode, hermes, будь-який
  ACP/A2A-клієнт) ходити в ACP-A2A_gateway: адреси, токени, формати message/send
  і session/prompt, продовження розмови через contextId, ізоляція між клієнтами,
  відомі обмеження. Тригер: "надіслати задачу через гейтвей", "поговорити з
  агентом X через шлюз", "A2A message/send", "чому continue таймаутить".
---

> **Мова:** українська · [English version](./SKILL.md) · [Русская версия](./SKILL-ru.md)

# gateway-client — як агенту користуватися ACP-A2A_gateway

Гейтвей (`GG-QandV/ACP-A2A_gateway`) — маршрутизатор між клієнтами та
ACP-агентами. Два порти, чотири напрямки. Ти можеш бути і клієнтом
(ходити в агентів через шлюз), і агентом (тебе підключають як stdio-агента).

## 0. Які агенти розмовляють ACP

Реєструй будь-якого з них як запис `agents[]`; шлюз піднімає процес і
розмовляє ACP через його stdio.

| Агент | Як віддає ACP |
|---|---|
| opencode | нативно: `opencode acp` |
| hermes | нативно: `hermes acp` |
| Claude Code (Anthropic) | адаптер `claude-agent-acp` — <https://github.com/agentclientprotocol/claude-agent-acp> |
| Codex (OpenAI) | адаптер `codex-acp` — <https://github.com/agentclientprotocol/codex-acp> |
| Gemini CLI | нативний режим: `gemini --acp` |
| Cline | ACP CLI — <https://cline.bot/cli> |
| Cursor | режим ACP — <https://cursor.com/docs/cli/acp> |

Повний курирований список — [реєстр ACP-агентів](https://agentclientprotocol.com/get-started/registry).
Агент, що не розмовляє ані ACP, ані A2A (наприклад OpenClaw), як stdio-агента
не підключити: потрібен адаптер, або виведи його як A2A-сервер і йди через
напрямок 2. Специфічні для агента змінні середовища — в `agents[].env`
унутріш `config.yaml`; доступні на конкретному хості `agent_id` — ті, що
оголошені в його `config.yaml`.

## 1. Коли ти клієнт (ідеш в агентів через шлюз)

### TCP (ACP-клієнт, напрямки 1/2)

```bash
# listen порт (типово 8347), перший рядок — рукостискання, далі line-delimited ACP JSON-RPC
{ printf '%s\n' '{"token":"TOKEN","agent_id":"<agent_id>"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}'
} | nc 127.0.0.1 8347
```

Кожний `session/new` дає **нову** сесію на тому ж процесі агента — можна тримати
багато розмов через один TCP-потік. Відповіді — JSON-рядки, корельовані за `id`.

### HTTP (A2A-клієнт, напрямки 3/4)

```bash
# http_listen порт (типово 8348)
curl -s http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json \
  -H "Authorization: Bearer <token>"

curl -s -X POST http://127.0.0.1:8348/agents/<agent_id>/rpc \
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
        "message":{"role":"user","parts":[{"kind":"text","text":"привіт"}]}
      }}'
```

## 2. Формати — критично, перевірено на живих агентах

### A2A `message/send`

- `message.parts[].kind` — `text` / `file` / `data` (tagged enum, kebab/lowercase).
- Відповідь: `result.context_id` (**snake_case**), `result.id` (task), `result.status`,
  `result.artifacts`.
- **Продовження розмови**: `contextId` кладеться в `message.contextId`
  (або `params.contextId`), а **НЕ** в `configuration` — configuration його не читає.
- Без `contextId` → шлюз починає нову розмову.
- Чужий `contextId` → помилка `contextId принадлежит другому клиенту`
  (ізоляція за токенами, IDOR закритий).
- Неіснуючий `contextId` → не відхиляється, створюється новий (за задумом).
- Сесія живе 24 год без діла; ліміт 256 розмов на агента.

### ACP `session/prompt` (якщо говориш з агентом напряму через TCP)

- `prompt` — це **послідовність** ContentBlocks, а не рядок (рядок → `-32602`).
- Блоки пишуться з полем `type` (не `kind`): `{"type":"text","text":"..."}`.
- `initialize` → `session/new` (запам'ятай `sessionId`) → `session/prompt` з цим `sessionId`.

## 3. Коли тебе підключають як агента (stdio)

- Твоя команда має бути **ACP-точкою входу** агента:
  `opencode acp`, `hermes acp`, `gemini --acp` або бінар адаптера
  (`claude-agent-acp` / `codex-acp`). Це **не** `--bare` і **не** `--print`.
- Твій stdout має бути **справжнім pipe** (не файлом): `> log` валить Hermes
  з `Pipe transport is only for pipes`. Шлюз спавнить через `Stdio::piped()` — ок.
- Агентам у headless-режимі зазвичай потрібні прапорці, щоб не відкривати браузер і не
  переавторизовуватися на кожному спавні — передавай їх через `agents[].env`.
- Очікуй, що шлюз/клієнт смикає `session/new` **багаторазово** на одному процесі —
  це нормально, щоразу відповідай новим `sessionId`.

## 4. Відомі обмеження (не панікуй, це не твій баг)

| Симптом | Причина | Обхід |
|---|---|---|
| `continue` за contextId таймаутить (2-й `message/send` у ту ж сесію) | Дефект конвертера шлюзу (напрямок 4), відтворюється на stdio-агентах цього хоста | Тримати кожну запит у новій сесії (без contextId) або чинити шлюз; прямим `session/prompt` продовження працює |
| `Reply::Streaming` — помилка "Phase 1: streaming is not implemented" | Стрімінг не реалізований у конвертері | Тільки блокуючі виклики |
| Таймаут відповіді агента | `agent_call_timeout_secs` (типово 120) | Збільш у `config.yaml` |
| Помилка `-32010` / HTTP 409 на старому `contextId` | Агент помер і був перезапущений (P2-10): сесія належить попередньому поколінню процесу (`ContextLost`) | Повтори той самий виклик — створиться свіжа сесія. Повідомлення одноразове |
| Агент завис, але виглядає "живим" | `is_alive` ловить смерть, а не зависання | Впирається в `agent_call_timeout_secs`, збільш таймаут |

## 5. Швидка перевірка, що шлюз живий

```bash
ss -tlnp | grep -E "8347|8348"        # обидва порти слухають
curl -s http://127.0.0.1:8348/agents/<id>/.well-known/agent.json -H "Authorization: Bearer <token>"
```
