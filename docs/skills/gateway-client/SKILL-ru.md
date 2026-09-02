---
name: gateway-client
description: >-
  Как агенту (Claude Code, Gemini CLI, Codex, Cline, opencode, hermes, любой
  ACP/A2A-клиент) ходить в ACP-A2A_gateway: адреса, токены, форматы message/send
  и session/prompt, продолжение разговора по contextId, изоляция между
  клиентами, известные ограничения. Триггер: "отправить задачу через гатевей",
  "поговорить с агентом X через шлюз", "A2A message/send", "почему continue
  таймаутит".
---

> **Язык:** русский · [English version](./SKILL.md) · [Українська версія](./SKILL-uk.md)

# gateway-client — как агенту пользоваться ACP-A2A_gateway

Гатевей (`GG-QandV/ACP-A2A_gateway`) — маршрутизатор между клиентами и
ACP-агентами. Два порта, четыре направления. Ты можешь быть и клиентом
(ходить в агентов через шлюз), и агентом (тебя подключают как stdio-агента).

## 0. Какие агенты говорят по ACP

Регистрируй любого из них как запись `agents[]`; шлюз поднимает процесс и разговаривает
ACP через его stdio.

| Агент | Как отдаёт ACP |
|---|---|
| opencode | нативно: `opencode acp` |
| hermes | нативно: `hermes acp` |
| Claude Code (Anthropic) | адаптер `claude-agent-acp` — <https://github.com/agentclientprotocol/claude-agent-acp> |
| Codex (OpenAI) | адаптер `codex-acp` — <https://github.com/agentclientprotocol/codex-acp> |
| Gemini CLI | нативный режим: `gemini --acp` |
| Cline | ACP CLI — <https://cline.bot/cli> |
| Cursor | режим ACP — <https://cursor.com/docs/cli/acp> |

Полный курируемый список — [реестр ACP-агентов](https://agentclientprotocol.com/get-started/registry).
Агент, который не говорит ни по ACP, ни по A2A (например OpenClaw), как stdio-агента не
подключить: нужен адаптер, либо выводи его как A2A-сервер и иди через направление 2.
Специфичные агенту переменные окружения — в `agents[].env` внутри `config.yaml`;
доступные на конкретном хосте `agent_id` — те, что объявлены в его `config.yaml`.

## 1. Когда ты клиент (идёшь в агентов через шлюз)

### TCP (ACP-клиент, направления 1/2)

```bash
# порт listen (по умолч. 8347), первая строка — handshake, дальше ACP JSON-RPC построчно
{ printf '%s\n' '{"token":"TOKEN","agent_id":"<agent_id>"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}'
} | nc 127.0.0.1 8347
```

Каждый `session/new` даёт **новую** сессию на том же процессе агента — можно
вести много разговоров через один TCP-поток. Ответы — строки JSON, корреляция по `id`.

### HTTP (A2A-клиент, направления 3/4)

```bash
# порт http_listen (по умолч. 8348)
curl -s http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json \
  -H "Authorization: Bearer <token>"

curl -s -X POST http://127.0.0.1:8348/agents/<agent_id>/rpc \
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
        "message":{"role":"user","parts":[{"kind":"text","text":"привет"}]}
      }}'
```

## 2. Форматы — критично, проверено на живых агентах

### A2A `message/send`

- `message.parts[].kind` — `text` / `file` / `data` (tagged enum, kebab/lowercase).
- Ответ: `result.context_id` (**snake_case**), `result.id` (task), `result.status`,
  `result.artifacts`.
- **Продолжение разговора**: `contextId` кладётся в `message.contextId`
  (или `params.contextId`), **НЕ** в `configuration` — конфигурация его не читает.
- Без `contextId` → гатевей заводит новый разговор.
- Чужой существующий `contextId` → ошибка `contextId принадлежит другому клиенту`
  (изоляция между токенами, IDOR закрыт).
- Несуществующий `contextId` → не отклоняется, заводится заново (так задумано).
- Сессия живёт 24ч простоя; потолок 256 разговоров на агента.

### ACP `session/prompt` (если говоришь с агентом напрямую через TCP)

- `prompt` — **sequence** ContentBlock'ов, НЕ строка (строка → `-32602`).
- Блоки пишутся с полем `type` (не `kind`): `{"type":"text","text":"..."}`.
- `initialize` → `session/new` (запомнить `sessionId`) → `session/prompt` с этим `sessionId`.

## 3. Когда тебя подключают как агента (stdio)

- Твоя команда должна включать ACP-подкоманду:
  ACP-точка входа агента: `opencode acp`, `hermes acp`, `gemini --acp`
  или бинарь адаптера (`claude-agent-acp` / `codex-acp`). Это **не** `--bare`, **не** `--print`.
- Твой stdout должен быть **настоящим pipe** (не файл): `> log` роняет Hermes
  с `Pipe transport is only for pipes`. Шлюз спавнит через `Stdio::piped()` — ок.
- Агентам в headless-режиме обычно нужны флаги, чтобы не открывать браузер и не переавторизовываться
  на каждом спавне — передавай их через `agents[].env` в `config.yaml`.
- Ожидай, что шлюз/клиент дёргает `session/new` **многократно** на одном процессе —
  это нормально, каждый раз отвечай новым `sessionId`.

## 4. Известные ограничения (не паниковать, это не твой баг)

| Симптом | Причина | Обход |
|---|---|---|
| `continue` по contextId таймаутит (2-й `message/send` в ту же сессию) | Дефект конвертера шлюза (направление 4), воспроизводится на stdio-агентах этого хоста | Держать каждый запрос в новой сессии (без contextId) либо чинить шлюз; прямым `session/prompt` продолжение работает |
| `Reply::Streaming` — ошибка «Фаза 1: стриминг не реализован» | Стриминг не реализован в конвертере | Только блокирующие вызовы |
| Таймаут ответа агента | `agent_call_timeout_secs` (по умолч. 120) | Увеличить в `config.yaml` |
| Ошибка `-32010` / HTTP 409 на старом `contextId` | Агент умер и был переспавнен (P2-10): сессия относится к прошлому поколению процесса (`ContextLost`) | Повторить тот же вызов — заведётся свежая сессия. Пометка одноразовая |
| Агент падает, но «жив» (завис) | `is_alive` ловит смерть, не зависание | Упрётся в `agent_call_timeout_secs`, увеличить таймаут |

## 5. Быстрая проверка, что шлюз жив

```bash
ss -tlnp | grep -E "8347|8348"        # оба порта слушаются
curl -s http://127.0.0.1:8348/agents/<id>/.well-known/agent.json -H "Authorization: Bearer <token>"
```
