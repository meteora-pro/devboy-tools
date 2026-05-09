# Онбординг секрет-фреймворка

Первое включение secret framework на новой машине или в новом репозитории. Документ ведёт от «ничего не настроено» до «`devboy doctor --secrets` зелёный, манифест валидируется в CI».

> **Когда использовать**: после `npm install -g @devboy-tools/cli` и базового `devboy onboard` (см. [Quick start](../getting-started/quick-start.md)). Если в проекте уже есть `.devboy/secrets.toml` и вы хотите интерактивный wizard — запускайте `setup-secrets` через AI-агента, он автоматизирует те же шаги.

## Чек-лист готовности

- [ ] `devboy --version` отвечает (CLI установлен).
- [ ] OS keychain доступен (macOS Keychain / Windows Credential Manager / Linux Secret Service).
- [ ] Есть права на создание `~/.devboy/secrets/` и `<project>/.devboy/`.
- [ ] Понимаете, какие секреты нужны проекту (можно начать с одного — манифест расширяется по ходу).

---

## Шаг 1. Установка и проверка CLI

Если `devboy` уже на PATH — пропустите шаг.

```bash
npm install -g @devboy-tools/cli
devboy --version
```

Альтернатива через `cargo`:

```bash
cargo install devboy-cli
```

После установки убедитесь, что подсистема секретов доступна:

```bash
devboy secrets --help
```

Должны увидеть подкоманды `list`, `describe`, `validate`, `migrate`, `agent`, `ui`, `rotate`. Если их нет — обновите CLI до версии 0.26 или выше (минимальная версия с secret framework).

## Шаг 2. Подключение первого источника

По умолчанию CLI работает с OS keychain без настройки. Этого достаточно для одного разработчика на одной машине. Когда команда подключает 1Password, Vault или env-store — расширяете конфиг роутера. Начнём с keychain.

Создайте файл `~/.devboy/secrets/sources.toml` (если ещё нет):

```bash
mkdir -p ~/.devboy/secrets
```

```toml
# ~/.devboy/secrets/sources.toml
schema_version = 1

[[source]]
name = "default-keychain"
kind = "keychain"
# Никаких credentials не нужно — keychain сам управляет доступом.
```

Проверьте, что роутер видит источник:

```bash
devboy doctor --secrets --json | jq '.sources'
```

Должна быть запись `{"name": "default-keychain", "kind": "keychain", "available": true}`.

> **Если планируете 1Password / Vault**: добавьте отдельный `[[source]]` блок с типом `1password` или `vault`. Подробности — [docs/guide/secrets/local-vault.md](./local-vault.md) и `devboy secrets describe --source <name>`.

## Шаг 3. Манифест проекта

Манифест объявляет, какие секреты проект ожидает увидеть. Без манифеста framework не знает, на что смотреть, и `doctor` не сможет жаловаться на их отсутствие.

Создайте `<project>/.devboy/secrets.toml`:

```toml
# <project>/.devboy/secrets.toml
required = [
  "team/<provider>/api-key",
  "team/<provider>/deploy-token",
]

optional = [
  "personal/<provider>/feature-flag-token",
]

[overrides."team/<provider>/api-key"]
description = "Используется CI-пайплайном при публикации артефактов."
rotate_every_days = 90

[secret."sandbox/<provider>/local-token"]
description = "Только для локальных smoke-тестов; в репозитории не хранится."
retrieval_url = "https://example.invalid/<provider>/tokens"
pattern_id = "generic-bearer"
rotation_method = "manual"
```

Структура (см. [ADR-020](../architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md)):

- `required` — список путей в формате ADR-020 (`<scope>/<provider>/<purpose>`), без значений которых проект не работает.
- `optional` — пути, которые улучшают опыт (рассылки, фича-флаги), но не блокируют сборку.
- `[overrides."<path>"]` — точечные правки метаданных для пути из глобального индекса (`~/.devboy/secrets/index.toml`).
- `[secret."<path>"]` — полная декларация для пути, который существует только в этом проекте (sandbox-сценарии, локальные плейграунды).

> **Шаблоны вместо реальных имён**: в примерах используются плейсхолдеры `<provider>` / `<purpose>`. В реальном манифесте пишите конкретные сегменты (`team/jira/api-key`), но **никогда** не публикуйте на GitHub имена клиентов, внутренние коды команд или ID серверов — это утечка инфраструктуры. Для таких полей используйте `team/`-scope с обобщённым provider-именем.

## Шаг 4. Валидация манифеста

Каждый раз после правки манифеста — формат-валидация:

```bash
devboy secrets validate
```

Что проверяется (см. [ADR-021] §6):

- Каждый путь — валидный ADR-020 (3+ сегмента, kebab-case).
- `required` ∩ `optional` пусто.
- Поля `rotate_every_days`, `expires_at`, `pattern_id` — корректного типа.
- Если включён liveness-flag (`--liveness`), framework проверит, что для каждого `required`-пути в роутере зарегистрирован источник, способный выдать значение.

Включите команду в pre-commit hook или CI-pipeline:

```yaml
# .github/workflows/secrets.yml
- run: devboy secrets validate --strict
```

`--strict` превращает предупреждения о non-conformant путях (P10.1) в ошибки.

## Шаг 5. Обзор того, что фреймворк видит

```bash
devboy secrets list
```

Вывод — таблица с колонками `path`, `status`, `expires_at`, `provider`. Только метаданные, без значений.

Подробности по конкретному пути:

```bash
devboy secrets describe team/<provider>/api-key
```

Покажет `description`, `retrieval_url`, `rotation_method`, `last_rotated_at`, `pattern_id` и все `overrides`, применённые поверх глобального индекса.

JSON-режим для скриптов:

```bash
devboy secrets list --json | jq '.[] | select(.status == "missing")'
```

## Шаг 6. Заполнение значений

Значения попадают в keychain через `secrets ui` (TUI/GUI с диалогом ввода) или через AI-агента (`secrets_request_provision` MCP-инструмент). Выберите режим, который ближе:

- **Интерактивно (рекомендуется)**:

  ```bash
  devboy secrets ui --tui
  ```

  Откроется TUI с inventory-видом. Стрелки — навигация, Enter — открыть диалог provision для выбранного пути, `q` — выход. Для GUI-окружений CLI автоопределит наличие `$DISPLAY` / `$WAYLAND_DISPLAY` и поднимет egui.

- **Через AI-агента**: попросите агента «provision missing secrets» — он вызовет `setup-secrets` skill (см. [docs/guide/secrets/agent-protocol.md](./agent-protocol.md)).

- **Скриптом** (только для повторяющихся секретов в одной машине):

  ```bash
  devboy secrets rotate team/<provider>/api-key --from-stdin --yes <<<"<value>"
  ```

  Помечает время ротации и валидирует формат. Для первичной заливки эквивалент — поднять daemon и записать через provision-флоу; см. [docs/guide/secrets/local-vault.md](./local-vault.md).

После ввода значений `secrets list` покажет `status = provisioned` для тех путей, где value реально сохранилось в keychain.

## Шаг 7. Финальная проверка

```bash
devboy doctor --secrets
```

Зелёный итог:

- Все `required` пути — `provisioned`.
- Все провайдеры в `sources.toml` — `available`.
- Нет non-conformant путей в keychain (legacy-записи мигрированы — см. `devboy secrets migrate`).
- Daemon запущен (только если активно использовался `secrets ui` или MCP-инструменты).

Если что-то красное — `doctor` укажет точный код и подскажет команду для починки. Дальше — `repair` skill или раздел [Doctor](../configuration/doctor.md).

## Что дальше

- [`local-vault.md`](./local-vault.md) — настройка собственного локального хранилища (zeroize-on-drop файл с XChaCha20-Poly1305).
- [`agent-protocol.md`](./agent-protocol.md) — как AI-агент работает с секретами через MCP без доступа к значениям.
- [`source-plugin-protocol.md`](./source-plugin-protocol.md) — добавление собственного источника через subprocess-плагин.
- ADR-020 / ADR-021 / ADR-023 — формальные спецификации манифеста, роутера и UX-слоя.

[ADR-021]: ../architecture/adr/ADR-021-secret-source-router.md
