# `echo` — пример SecretSource subprocess-плагина

Маленький Python-плагин для проверки протокола из [docs/guide/secrets/source-plugin-protocol.md](../../docs/guide/secrets/source-plugin-protocol.md). На каждый `secret_source.get` возвращает `value = "echo:<reference>"`, не ходит в реальный backend, хорошо подходит для smoke-тестов и обучения.

## Что внутри

- `devboy-source-echo.py` — исполняемый скрипт, реализующий все пять методов протокола (`init`, `is_available`, `get`, `list`, `validate`).
- `devboy-source-echo.toml` — sidecar manifest. **Перед использованием обновите `checksum_sha256`** на реальный SHA-256 от вашей копии скрипта (см. ниже).

## Установка

```bash
mkdir -p ~/.devboy/plugins/secrets/
cp devboy-source-echo.py  ~/.devboy/plugins/secrets/
cp devboy-source-echo.toml ~/.devboy/plugins/secrets/
chmod +x ~/.devboy/plugins/secrets/devboy-source-echo.py

# Подсчитайте checksum и впишите в манифест
sha256sum ~/.devboy/plugins/secrets/devboy-source-echo.py
# → <hex>  ...
sed -i.bak \
  "s/^checksum_sha256 = .*/checksum_sha256 = \"<hex>\"/" \
  ~/.devboy/plugins/secrets/devboy-source-echo.toml
rm ~/.devboy/plugins/secrets/devboy-source-echo.toml.bak
```

## Smoke-test

После установки:

```bash
# Прямой ручной диалог, без хост-supervisor'а
echo '{"jsonrpc":"2.0","id":1,"method":"secret_source.init","params":{"source_name":"echo","config":{},"protocol_version":"1.0"}}' \
  | python3 ~/.devboy/plugins/secrets/devboy-source-echo.py
# → {"jsonrpc": "2.0", "id": 1, "result": {"source_name": "echo", "capabilities_bits": 7, "plugin_version": "0.1.0"}}

# Через хост-supervisor (P15.2): добавьте `[[source]] kind = "echo"`
# в ~/.devboy/secrets/sources.toml и прогоните:
devboy doctor --secrets --json | jq '.sources[] | select(.name == "echo")'
```

## Что происходит при `get`

```bash
echo '{"jsonrpc":"2.0","id":2,"method":"secret_source.get","params":{"reference":"demo/team/echo/api-key"}}' \
  | python3 devboy-source-echo.py
# → {"jsonrpc": "2.0", "id": 2, "result": {"value": "echo:demo/team/echo/api-key", "lease_seconds": null}}
```

В реальном плагине здесь был бы запрос к backend'у. Echo возвращает префикс + reference — этого хватает, чтобы убедиться, что router нашёл плагин и pipeline до значения работает.

## Чему учит этот пример

1. **Wire-format в одном файле** — 130 строк Python показывают весь протокол целиком; читать намного быстрее, чем спецификацию.
2. **Capability bitmask** — берёте `CAP_READ | CAP_LIST | CAP_VALIDATE` и заявляете в `init`. Хост дальше не попросит того, что вы не объявили.
3. **Init-first contract** — после spawn первая команда обязана быть `init`. Любая другая возвращает `error.kind = "other"`.
4. **Где не надо логировать value** — посмотрите на `secret_source.get` в коде: `value` собирается только в response, никаких `print(value)` нет. Это правило протокола, не «лучшая практика».
5. **Graceful EOF** — main loop читает stdin построчно через `for line in sys.stdin`. Когда хост закроет stdin (shutdown), цикл завершится сам и процесс выйдет.

## Что adapt'ировать для своего плагина

- Замените `handle()` ветку для `secret_source.get` — вместо `f"echo:{reference}"` ходите в реальный backend.
- В `secret_source.list` верните реальный inventory.
- Если backend поддерживает leases — заполните `lease_seconds` в `get`-результате.
- Если что-то не поддерживаете (например, нет enumeration) — верните `error.kind = "unsupported-capability"` и не объявляйте `CAP_LIST` в `init`.
- Обновите `name`, `version`, `allowed_env_vars` в TOML под свой плагин.
- Подсчитайте новый checksum: `sha256sum devboy-source-<your-name>.py`.

## Лайфтайм-сюрпризы

Хост убивает плагин после **60 секунд** простоя (см. `crates/devboy-storage/src/plugin_client.rs::LifetimePolicy::default`). После этого следующий запрос spawn'ит свежий процесс, и `init` поедет заново. Если ваш плагин держит долгий state в памяти (open connection, кэш) — будьте готовы к тому, что его придётся восстанавливать в `init` после каждого reap'а. В лучшем варианте — храните state во внешнем store, init остаётся дешёвым.

Также, после трёх падений в течение минуты хост помечает плагин как `Disabled` и перестаёт его поднимать (restart cap). Восстанавливается через `devboy secrets agent reset --plugin <name>` (когда команда появится) или перезапуском хоста. Чтобы избежать — обрабатывайте exception'ы внутри плагина и возвращайте `error.kind = "other"` вместо краша.
