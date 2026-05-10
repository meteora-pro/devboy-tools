# Agent protocol: работа с секретами через MCP

> Translation note: the canonical version is the English [`agent-protocol.md`](../agent-protocol.md) (per the OSS-docs-in-English rule). This Russian copy is kept for the team; if it diverges, the English version is authoritative.

Для авторов AI-агентов и MCP-клиентов. Документ объясняет инвариант «agent never sees the value» (ADR-023 §3.7), полный перечень MCP-инструментов в семействе `secrets_*`, их семантику, лайфтайм запросов и формат ответов.

См. [ADR-023](../../architecture/adr/ADR-023-secret-store-ux-layer.md) §3.7 для формальной спецификации и `crates/devboy-mcp/src/secrets_tool.rs` + `secrets_provision.rs` для реализации.

---

## Базовый инвариант

> **Агент никогда не видит значения секретов. Только метаданные.**

Это не «лучшая практика» и не runtime-проверка — это типовая граница в коде. Reply-структуры всех инструментов `secrets_*` физически не имеют поля `value`. Если в будущем рефакторинг добавит `SecretString` в reply — компиляция упадёт на маркерном трейте `AgentSafeReply`. Дополнительно работают:

- **CI grep gate** в `crates/devboy-mcp/tests/no_expose_secret_outside_allowlist.rs`. Любой `.expose_secret()` вне allowlist → fail.
- **Negative integration test** в `crates/devboy-mcp/tests/secret_tool_responses_never_leak_value.rs`. Прогоняет фикстурный sentinel через каждый `secrets_*` инструмент и проверяет, что значения нет в ответе.

На стороне агента это значит: **нельзя написать `secrets.get`** и получить токен. Если нужно использовать значение — оно достаётся через high-level provider tool (например, `get_issues` с уже разрешённым в proxy токеном). Обход доступен только пользователю — через UI-диалог, который никогда не возвращается агенту в виде строки.

---

## Перечень инструментов

| Tool name | Назначение | Возвращает | Lifecycle |
|---|---|---|---|
| `secrets_list` | Просмотр inventory активного контекста | Список метаданных | Синхронный |
| `secrets_describe` | Детальная карточка одного пути | Метаданные одного пути | Синхронный |
| `secrets_request_provision` | Открыть UI-диалог для первичного ввода значения | `request_id` | Асинхронный, polling |
| `secrets_request_rotation` | То же, но с destructive-confirm для замены | `request_id` | Асинхронный, polling |
| `secrets_propose_metadata` | Предложить правки метаданных существующего пути | `request_id` | Асинхронный, polling |
| `secrets_propose_new_path` | Предложить регистрацию нового пути в манифест | `request_id` | Асинхронный, polling |
| `secrets_poll_status` | Получить статус выданного `request_id` | `{kind, status, age_seconds, …}` | Синхронный |

Запросы UI-диалогов — асинхронные: агент инициирует диалог и опрашивает статус. Пользователь в свободном темпе вводит значение в окно/TUI; агент видит только ход событий.

---

## `secrets_list`

Перечислить пути, объявленные в манифесте активного контекста.

### Запрос

```json
{
  "name": "secrets_list",
  "arguments": {
    "path_contains": "jira",
    "scope": "team",
    "status": "expiring",
    "include_internal": false
  }
}
```

Все аргументы опциональны. Фильтры комбинируются по AND. `include_internal` по умолчанию `false` — внутренние пути (`__sys/...`) скрыты.

### Ответ

```json
[
  {
    "path": "team/<provider>/api-key",
    "status": "expiring",
    "expires_at": "2026-05-23",
    "source_name": null,
    "capabilities_hint": "read,rotate"
  }
]
```

Поля:

- `path` — ADR-020 путь (`<scope>/<provider>/<purpose>`).
- `status` — `registered` / `expiring` (≤14 дней до истечения) / `expired`. Считается по `expires_at`, не по live-probe источника. Это умышленно: probe мог бы случайно раскрыть факт отзыва токена пользователем.
- `expires_at` — ISO-8601 (`YYYY-MM-DD`) или `null`.
- `source_name` — на текущий момент всегда `null` (роутер не пробрасывается в MCP-сервер; field зарезервирован в wire-format).
- `capabilities_hint` — `"read"` (manual rotation) или `"read,rotate"` (provider-ui / provider-api).

Ответ отсортирован по `path` для стабильности тестов.

---

## `secrets_describe`

Карточка одного пути.

### Запрос

```json
{
  "name": "secrets_describe",
  "arguments": { "path": "team/<provider>/api-key" }
}
```

### Ответ

Строгий superset `secrets_list` элемента + дополнительные поля метаданных:

```json
{
  "path": "team/<provider>/api-key",
  "status": "registered",
  "expires_at": "2026-12-01",
  "source_name": null,
  "capabilities_hint": "read",
  "description": "API token used by team CI",
  "retrieval_url": "https://example.invalid/<provider>/tokens",
  "rotation_method": "manual",
  "last_rotated_at": "2026-04-15",
  "rotate_every_days": 90,
  "pattern_id": "<pattern-id>"
}
```

Ошибки:

- `not-found` — путь не виден в активном контексте (ни в проектном манифесте, ни в глобальном индексе с overrides).
- `invalid-path` — синтаксис не соответствует ADR-020.
- `merge-failed` — конфликт в манифесте (дубликат entry, недопустимая структура).

> **Manifest-gating**: видны только пути, на которые ссылается активный проектный манифест. Глобальный индекс не утекает целиком.

---

## `secrets_request_provision`

Попросить framework открыть UI-диалог, в который пользователь введёт **новое** значение секрета.

### Запрос

```json
{
  "name": "secrets_request_provision",
  "arguments": {
    "path": "team/<provider>/api-key",
    "mode": "provision"
  }
}
```

`mode` опционален, по умолчанию `"provision"`. Допустимое значение `"rotation"` форсирует destructive-confirm — но обычно для ротации удобнее использовать отдельный `secrets_request_rotation` (см. ниже).

### Ответ

```json
{ "request_id": "prov-1f9b3a2c5e7d" }
```

`request_id` — opaque строка вида `prov-<12-hex>`. Хранить и опрашивать через `secrets_poll_status`.

### Lifecycle

```
secrets_request_provision  ──►  request_id, status = pending
                                       │
                                       │  (пользователь вводит значение в диалоге)
                                       ▼
                                daemon stores value, status = ok
                                       │
secrets_poll_status(request_id) ──►   status = ok
                                       │
agent ──► high-level provider tool (e.g. get_issues),
          token уже доступен через proxy alias
```

Запрос expirится через **5 минут**, если пользователь не нажал ни Save, ни Cancel. Агент должен предусмотреть таймаут на свой polling.

---

## `secrets_request_rotation`

Та же семантика, что `request_provision { mode: "rotation" }`, но без двусмысленности на стороне вызова. Инструмент сам передаёт `mode = Rotation` в registry; UI поднимает диалог с явным destructive-confirm checkbox («I understand this overwrites the current secret»).

### Запрос

```json
{
  "name": "secrets_request_rotation",
  "arguments": { "path": "team/<provider>/api-key" }
}
```

### Ответ

```json
{ "request_id": "prov-7c3e5a2d8b9f" }
```

Lifecycle — идентичен `secrets_request_provision`. Используйте `secrets_poll_status` для отслеживания.

---

## `secrets_propose_metadata`

Предложить пользователю правки метаданных существующего пути. Открывается edit-metadata диалог с **diff preview**: текущие значения слева читаются из манифеста (надёжный источник), proposed — справа из payload агента.

### Запрос

```json
{
  "name": "secrets_propose_metadata",
  "arguments": {
    "path": "team/<provider>/api-key",
    "fields": {
      "description": "Updated description",
      "retrieval_url": "https://example.invalid/<provider>/new-url",
      "rotate_every_days": 60,
      "expires_at": "2027-01-01",
      "pattern_id": "<new-pattern-id>"
    }
  }
}
```

Поля в `fields` — все опциональные. Опущенные поля не предлагаются к изменению; в diff они остаются прежними.

### Ответ

```json
{ "request_id": "prov-9a2b8c4f7e1d" }
```

### Trust boundary против prompt injection

Это критическая часть протокола. UI рендерит **только** `current` колонку из манифеста — строки агента появляются исключительно в `proposed`. Это значит:

- Агент не может «переписать» метаданные через хитро составленные значения, которые выглядят как существующие.
- Пользователь видит точно, что предложено к изменению.
- Любая попытка «подменить» путь через description со специальными символами разбивается о то, что путь рендерится из `manifest`, а не из payload.

Для деталей — секция «Trust boundary» в `crates/devboy-mcp/src/secrets_provision.rs`.

---

## `secrets_propose_new_path`

Предложить **регистрацию нового пути** в манифесте проекта. UI открывает discovery-style диалог с `suggested_path` как редактируемой стартовой точкой.

### Запрос

```json
{
  "name": "secrets_propose_new_path",
  "arguments": {
    "suggested_path": "team/<provider>/<new-purpose>",
    "metadata": {
      "description": "Newly discovered credential",
      "pattern_id": "<pattern-id>",
      "retrieval_url": "https://example.invalid/<provider>/tokens",
      "rotate_every_days": 90,
      "expires_at": null
    }
  }
}
```

### Ответ

```json
{ "request_id": "prov-2e4f6a8c1b3d" }
```

В отличие от `propose_metadata`, путь редактируем — пользователь может отказаться от предложения агента и выбрать собственное имя. Финальное решение остаётся за человеком.

---

## `secrets_poll_status`

Опрос статуса любого `request_id` от любого `request_*` или `propose_*` инструмента. Один endpoint — общая семантика лайфтайма.

### Запрос

```json
{
  "name": "secrets_poll_status",
  "arguments": { "request_id": "prov-1f9b3a2c5e7d" }
}
```

### Ответ

```json
{
  "request_id": "prov-1f9b3a2c5e7d",
  "path": "team/<provider>/api-key",
  "kind": "provision",
  "status": { "kind": "ok" },
  "age_seconds": 27
}
```

Поля:

- `request_id` — эхо запроса.
- `path` — путь, по которому изначально открылся диалог. Эхо для подтверждения.
- `kind` — `provision` / `rotation` / `metadata-proposal` / `new-path-proposal`.
- `status.kind` — один из:
  - `pending` — диалог открыт, пользователь ещё не нажал Save или Cancel.
  - `ok` — пользователь сохранил значение / принял proposal.
  - `cancelled` — пользователь закрыл диалог без сохранения.
  - `expired` — 5-минутный TTL истёк, registry пометил запись как Expired.
  - `failed { reason }` — диалог не открылся (нет launcher'а, daemon down и т.п.) или provider отклонил submission.
- `age_seconds` — сколько времени прошло с момента создания запроса.

Если `request_id` не существует (был sweep'нут), ответ — error `unknown request_id: <id>`.

### Polling pattern

```text
loop:
  resp = secrets_poll_status(request_id)
  if resp.status.kind == "pending":
    sleep(2..5 seconds)
    continue
  break

handle resp.status.kind:
  ok        → proceed (e.g. retry the high-level tool that needed this secret)
  cancelled → ask user "what now?" — retry, abandon, etc.
  expired   → tell user the dialog timed out; offer a fresh request
  failed    → show resp.status.reason; suggest devboy secrets agent start
```

Не делайте polling чаще раза в 2 секунды — диалог не быстрее, чем человек печатает. Tight loop разогревает CPU и telemetry, но не приближает результат.

---

## Полный сценарий: provision + retry

```text
User: "Use Jira to fetch the latest issues."

Agent: get_issues({"limit": 10})
       → ProviderError: missing token at team/<provider>/api-key

Agent thinks: secret missing, request provision.
Agent: secrets_describe({"path": "team/<provider>/api-key"})
       → { ... description, retrieval_url, capabilities_hint: "read", ... }

Agent: secrets_request_provision({"path": "team/<provider>/api-key"})
       → { "request_id": "prov-1f9b3a2c5e7d" }

Agent → User: "I've opened a dialog for the missing token. Paste the value
              from <retrieval_url>, then click Save."

# Agent polls every ~3 seconds
Agent: secrets_poll_status({"request_id": "prov-1f9b3a2c5e7d"})
       → { ..., "status": { "kind": "pending" }, "age_seconds": 12 }
... (повтор) ...
Agent: secrets_poll_status(...)
       → { ..., "status": { "kind": "ok" }, "age_seconds": 47 }

Agent → User: "Got it, retrying."
Agent: get_issues({"limit": 10})
       → 10 issues returned
```

Обратите внимание: агент **никогда** не запрашивает значение токена. Он узнаёт только то, что save случился; токен попал из диалога в daemon, оттуда — в proxy alias, и `get_issues` подхватил его через тот же proxy без участия агента.

---

## Чего на agent surface **нет**

Эти инструменты **не существуют** и не появятся:

- ❌ `secrets_get` / `secret.get` — выдача значения агенту. Замена: high-level provider tool с алиасом.
- ❌ `secrets_set` / `secret.put` — прямая запись из payload агента. Замена: `secrets_request_provision` → пользователь вводит руками.
- ❌ `secrets_export` / dump — массовая выгрузка. Не предусмотрено архитектурой.

Если new-tool кажется удобной упрощалкой — посмотрите ещё раз. Удобство «передать токен напрямую» противоречит инварианту, и framework не предложит обходного пути.

## Допустимые operational tools (не агентский surface)

Для DevOps / setup-сценариев, где удобство выше, чем agent-safety, есть отдельные CLI-команды (доступные только локально, не через MCP):

- `devboy secrets validate` — формат-чек манифеста и live-probe источников.
- `devboy secrets rotate <path>` — интерактивная ротация для разработчика (P13.1).
- `devboy secrets ui` — открыть TUI/GUI inventory для ручного просмотра/правок (P12.2).

Эти команды требуют присутствия человека за терминалом и не доступны через JSON-RPC. Агент может *рекомендовать* пользователю их запустить, но сам выполнить не может.

---

## Версионирование протокола

`PROTOCOL_VERSION = "1.0"` (см. `crates/devboy-storage/src/plugin_protocol.rs`). Семантический breakdown:

- **Major bump (2.0)** — поля reply убраны/переименованы, поведение `kind` enum не совместимо. Старые агенты получат ошибку при попытке использовать новый сервер.
- **Minor bump (1.x)** — добавление новых полей (всегда optional), новых вариантов status (агент должен трактовать неизвестные kind как `failed`), новых инструментов в семейство `secrets_*`.
- **Patch (1.x.y)** — bugfix без изменения wire-format.

Для агентского кода рекомендация: парсите ответы tolerant — игнорируйте неизвестные поля, неизвестные `status.kind` интерпретируйте как `failed`. Это даёт совместимость с будущими minor-версиями без обновления агента.

---

## См. также

- [`onboarding.md`](../onboarding.md) — настройка манифеста и роутера, без которой `secrets_list` ничего не покажет.
- [`local-vault.md`](../local-vault.md) — где значения физически живут после save из диалога.
- [`source-plugin-protocol.md`](../source-plugin-protocol.md) — как добавить собственный источник, который видят агентские инструменты.
- ADR-023 §3.7 — формальная спецификация trust boundary и инвариантов.
- `crates/devboy-mcp/src/secrets_tool.rs` + `secrets_provision.rs` — исходный код реализации.
