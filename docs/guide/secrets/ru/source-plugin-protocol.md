# Source plugin protocol: пишем собственный SecretSource

> Translation note: the canonical version is the English [`source-plugin-protocol.md`](../source-plugin-protocol.md) (per the OSS-docs-in-English rule). This Russian copy is kept for the team; if it diverges, the English version is authoritative.

Документ для авторов community-плагинов, которые расширяют список источников секретов в `devboy-tools` за пределами поставляемых из коробки (keychain, local-vault, 1Password, Vault, env-store). Описывает stdio JSON-RPC wire-format, sidecar manifest, discovery и контракт жизненного цикла.

См. [ADR-021](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md) §6 (трейт `SecretSource`) и §10 (subprocess plugin extension), а также `crates/devboy-storage/src/plugin_protocol.rs`, `plugin_manifest.rs`, `plugin_client.rs` для эталонной реализации хоста.

---

## Зачем это нужно

Стандартные источники покрывают типичные кейсы, но если ваша команда хранит секреты в:

- внутреннем self-hosted KV-сервере, не совместимом с Vault HTTP API,
- проприетарном HSM с CLI-обёрткой,
- legacy-pipeline'е, где значения берутся из специфичной БД,

то вместо ожидания фичи в upstream-репозитории удобнее написать subprocess-плагин. Хост запускает плагин лениво и общается с ним через stdio JSON-RPC; плагин может быть на любом языке (Python, Go, shell — что угодно, что умеет читать stdin построчно и писать в stdout).

## High-level архитектура

```text
┌──────────────────────────────────┐
│ devboy host (CLI / daemon / MCP) │
└──────────────────────────────────┘
                 │
                 ▼ ленивый spawn по первому запросу
┌──────────────────────────────────────────────────┐
│ ~/.devboy/plugins/secrets/devboy-source-<name>   │
│  ↑↓  newline-delimited JSON-RPC по stdin/stdout  │
└──────────────────────────────────────────────────┘
                 │
                 ▼
       реальное хранилище (HSM, custom KV, …)
```

Хост держит один subprocess на плагин на всё время сессии (с idle-reaper'ом — см. лайфтайм), отправляет команды через stdin и читает ответы из stdout. Никаких сетевых сокетов, никакого FFI — только труба.

## Wire-format: JSON-RPC 2.0 поверх stdin/stdout

Каждый кадр — одна строка с одним JSON-объектом, заканчивающаяся `\n`. Запросы и ответы шлются строго в порядке (один запрос — один ответ; конкурентность не предусмотрена).

### Запрос (host → plugin)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "secret_source.init",
  "params": {
    "source_name": "<name>",
    "config": { "address": "https://example.invalid/<service>" },
    "protocol_version": "1.0"
  }
}
```

- `jsonrpc` — всегда строка `"2.0"`. Любое другое значение — ответ должен быть error.
- `id` — целое число (u64). Хост гарантирует уникальность в пределах одного процесса.
- `method` — одно из пяти имён ниже.
- `params` — объект, специфичный для метода. Пустой объект (`{}`) или отсутствие поля для методов без параметров.

### Успешный ответ (plugin → host)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "source_name": "<name>",
    "capabilities_bits": 1,
    "plugin_version": "0.1.0"
  }
}
```

`id` обязан соответствовать `id` запроса. Хост сравнивает их и падает, если не сходится.

### Ответ с ошибкой

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "kind": "needs-credential",
    "detail": "op signin required for account <id>"
  }
}
```

Поле `result` и `error` взаимоисключающие — ровно одно из них в каждом ответе.

## Пять методов

### `secret_source.init`

Первый вызов после spawn. Хост передаёт имя источника и конфиг из `sources.toml`; плагин возвращает свои capabilities.

**Request params**:

```json
{
  "source_name": "<name>",
  "config": { "<arbitrary": "<config>" },
  "protocol_version": "1.0"
}
```

**Response result**:

```json
{
  "source_name": "<name>",
  "capabilities_bits": 1,
  "plugin_version": "<version>"
}
```

`capabilities_bits` — битовая маска (см. `crates/devboy-storage/src/source.rs::Capabilities`):

| Bit | Capability | Что заявляете |
|---|---|---|
| `0b0000_0001` | `READ` | Поддерживаете `secret_source.get` |
| `0b0000_0010` | `LIST` | Поддерживаете `secret_source.list` |
| `0b0000_0100` | `VALIDATE` | Поддерживаете `secret_source.validate` |
| `0b0000_1000` | `WRITE` | Резерв (зарегистрирован, ещё не используется) |
| `0b0001_0000` | `ROTATE` | Резерв |
| `0b0010_0000` | `BIOMETRIC_PROMPT` | Источник может запросить биометрию (1Password Touch ID) |
| `0b0100_0000` | `AUDIT_LOGGED` | Источник пишет audit log при каждом доступе |

Если плагин поддерживает только READ — отправляйте `"capabilities_bits": 1`.

Хост отказывается продолжать, если `protocol_version` в запросе старше major-версии, чем плагин понимает.

### `secret_source.is_available`

Проверка готовности. Дёшевая операция, должна возвращаться быстро (< 100 мс желательно).

**Request params**: нет (пустой объект `{}` или отсутствие поля).

**Response result**:

```json
{
  "status": "available",
  "detail": null
}
```

Возможные `status`:

- `available` — плагин готов отвечать на `get`/`list`/`validate`.
- `unavailable` — backend недоступен (network down, файл повреждён). `detail` — короткое описание для `doctor`.
- `needs-credential` — плагин запущен, но его credential отсутствует или истёк (`op signin` нужен). `detail` — что именно сделать.

### `secret_source.get`

Получить значение по reference.

**Request params**:

```json
{ "reference": "secret/data/team/<provider>/api-key" }
```

**Response result**:

```json
{
  "value": "<plaintext>",
  "lease_seconds": 3600
}
```

`value` — plaintext. **Это единственное место в протоколе, где значение проходит по wire**. Хост сразу оборачивает его в `secrecy::SecretString` и zeroize-ит после использования. Плагин обязан не логировать `value`.

`lease_seconds` — опционально, если backend поддерживает leases (Vault dynamic secrets). Хост использует это значение как верхнюю границу TTL для своего cache.

Если значения нет — возвращайте `error.kind = "bad-reference"` с полем `reference: "<reference>"` и `reason: "not found"`.

### `secret_source.list`

Перечислить inventory backend'а. Используется discovery-flow в TUI/GUI и `secrets_propose_new_path` MCP-инструментом.

**Request params**: нет.

**Response result**:

```json
{
  "entries": [
    {
      "reference": "secret/data/team/<provider>/api-key",
      "display": "Team <provider> API key"
    },
    {
      "reference": "secret/data/team/<provider>/deploy-token",
      "display": null
    }
  ]
}
```

Если backend не поддерживает enumeration — верните `error.kind = "unsupported-capability"` с `capability: "list"`.

### `secret_source.validate`

Проверить, что reference хорошо сформирован, без round-trip за значением. Дешёвый sanity-check.

**Request params**:

```json
{ "reference": "secret/data/team/<provider>/api-key" }
```

**Response result**:

```json
{ "ok": true }
```

`ok = false` — нештатно; ошибки сообщаются через `error.kind = "bad-reference"`.

## Error-варианты

```json
{ "kind": "unavailable",          "detail": "<...>" }
{ "kind": "unsupported-capability", "capability": "<list|write|...>" }
{ "kind": "bad-reference",        "reference": "<r>", "reason": "<...>" }
{ "kind": "needs-credential",     "detail": "<...>" }
{ "kind": "other",                "detail": "<...>" }
```

Хост маппит их на свой `SourceError` enum один-в-один. `other` — для всего, что не подпадает под остальные варианты (transport timeout, parse error, …).

## Sidecar manifest

Для каждого плагина рядом с исполняемым файлом лежит TOML-описание:

```text
~/.devboy/plugins/secrets/
├── devboy-source-<name>.toml      # sidecar manifest
└── devboy-source-<name>           # executable
```

### Формат `devboy-source-<name>.toml`

```toml
name = "<name>"
version = "0.1.0"
executable = "devboy-source-<name>"
allowed_env_vars = ["HOME", "PATH"]
checksum_sha256 = "<lower-case-hex-digest-of-executable>"
```

Поля:

- `name` — короткое идентификационное имя. Должно совпадать с `<name>` в имени файла manifest'а — иначе хост откажется загружать.
- `version` — advisory; логируется и показывается в `doctor`. Не используется для семантической совместимости.
- `executable` — путь относительно директории манифеста (или абсолютный). Хост канонизирует и проверяет, что файл существует.
- `allowed_env_vars` — единственные env-vars, которые попадут в дочерний процесс. Хост вызывает `Command::env_clear()` перед exec, потом добавляет ровно эти переменные. Всё остальное (включая `$AWS_SECRET_KEY`) скрыто.
- `checksum_sha256` — SHA-256 hex (case-insensitive) от байтов executable. Хост пересчитывает и отказывается запускать при несоответствии.

### Где лежит

Дефолтная директория discovery — `$HOME/.devboy/plugins/secrets/`. Хост сканирует её при старте, ищет файлы, начинающиеся на `devboy-source-` и заканчивающиеся на `.toml`. Каждый найденный manifest парсится независимо — одна сломанная конфигурация не отключает остальные плагины.

## Контракт жизненного цикла

См. ADR-021 §10 для полной формулировки. Кратко:

| Параметр | Default | Описание |
|---|---|---|
| **Lazy spawn** | — | Плагин не запускается, пока хосту реально не понадобится первый запрос. |
| **Idle timeout** | 60 секунд | Если плагин не использовался дольше — хост убивает процесс перед следующим запросом и спавнит заново. |
| **Shutdown grace** | 10 секунд | При остановке хост сначала шлёт `SIGTERM`, ждёт grace, потом `SIGKILL`. |
| **Restart cap** | 3 крэша / 60 секунд | Если плагин падает чаще — переходит в state `Disabled`. Дальнейшие запросы отказываются без spawn. Сброс — оператором через `doctor`. |
| **Env restriction** | `allowed_env_vars` из манифеста | Дочерний процесс видит только перечисленные переменные. |

Что это значит для автора плагина:

- **Не предполагайте долгого uptime**. Любой долговременный state хранится во внешнем backend'е (KV-сервере, файле), а не в памяти процесса. После idle-reap state теряется.
- **Init должен быть дешёвым**. Каждый ленивый spawn — это `init` + первая операция. Если init читает 100 МБ кеша с диска — пользователь увидит лаг при каждом первом обращении.
- **Crash-безопасность**. Падение интерпретируется хостом как краш, считается в restart cap. Ловите exception'ы в плагине и возвращайте `error.kind = "other"` с понятным `detail` вместо паники.
- **Корректный shutdown**. Когда хост закроет ваш stdin — это сигнал к graceful exit. Очистите ресурсы и вернитесь из main loop.

## Sample plugin: echo-source

В репозитории есть рабочий пример `examples/secrets-source-echo/`. Это Python-скрипт, который:

- Принимает init и заявляет capabilities `READ | LIST | VALIDATE`.
- На `get` возвращает `value = format!("echo:{reference}")` — то есть значение совпадает с reference. Удобно для smoke-тестов и обучения протоколу.
- На `list` отдаёт три фейковых entries.
- На `validate` принимает любые непустые references.

См. [`examples/secrets-source-echo/README.md`](https://github.com/meteora-pro/devboy-tools/blob/main/examples/secrets-source-echo/README.md) для инструкций по сборке + установке + проверке.

## Чек-лист для нового плагина

1. [ ] Реализуйте main loop: построчное чтение stdin, парс JSON, dispatch по `method`, печать одной строки JSON ответа в stdout.
2. [ ] Покройте все пять методов или возвращайте `unsupported-capability` для тех, что не поддерживаете.
3. [ ] Заявите честный `capabilities_bits` в ответ на `init` — иначе хост попросит то, что вы не сделаете.
4. [ ] Никогда не логируйте `value` из `secret_source.get`. Никогда.
5. [ ] Обработайте signal (SIGTERM / Ctrl+D) корректно — graceful exit без зависаний.
6. [ ] Напишите sidecar TOML с правильным `name`, `executable`, `allowed_env_vars`, `checksum_sha256`.
7. [ ] Положите оба файла в `~/.devboy/plugins/secrets/`.
8. [ ] Прогоните `devboy doctor --secrets`. Должен увидеть ваш плагин в списке источников.
9. [ ] Прогоните `devboy secrets describe --source <name>` (когда метод появится) или MCP-инструмент `secrets_request_provision` против пути, который роутится в ваш плагин.

## См. также

- [`onboarding.md`](../onboarding.md) — как добавить custom-источник в `sources.toml`, чтобы router его использовал.
- [`agent-protocol.md`](../agent-protocol.md) — как агент видит ваш источник через MCP-инструменты.
- ADR-021 §6 — формальный контракт `SecretSource` trait, на который маппится этот wire-protocol.
- ADR-021 §10 — лайфтайм-контракт subprocess plugin'ов с обоснованием defaults.
- `crates/devboy-storage/src/plugin_protocol.rs` — типы wire-format в Rust.
- `crates/devboy-storage/src/plugin_manifest.rs` — sidecar parser + checksum verification.
- `crates/devboy-storage/src/plugin_client.rs` — host-side supervisor с lazy spawn / idle reap / restart cap.
