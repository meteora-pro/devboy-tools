# Local Vault: формат файла, recovery и резервное копирование

> Translation note: the canonical version is the English [`local-vault.md`](../local-vault.md) (per the OSS-docs-in-English rule). This Russian copy is kept for the team; if it diverges, the English version is authoritative.

Local vault — собственное локальное хранилище зашифрованных секретов из `devboy-tools`. Один файл, три способа разблокировки, recovery-фраза для аварий. Документ объясняет формат, когда выбирать local-vault вместо OS keychain или внешних источников, и как правильно бэкапить.

См. [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md) §3.1–§3.3 и [ADR-021](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md) §4 для полной спецификации.

---

## Когда использовать local-vault

| Сценарий | Рекомендуемый источник | Почему |
|---|---|---|
| Один разработчик, одна машина, macOS/Win/Linux | **OS keychain** | Уже есть, ничего ставить не нужно, биометрия из коробки. |
| Команда разработчиков с общими токенами (CI, deploy) | **1Password / Vault** | Централизованное управление, аудит, ротация. |
| Серверный CI без интерактивного логина | **env-store** (env vars) или **vault** (HTTP KV v2) | Без UI, без интерактивности. |
| Локальный playground без доступа в keychain (контейнер, devcontainer, bare Linux без gnome-keyring) | **local-vault** | Один файл — легко прокинуть в контейнер, не зависит от системных сервисов. |
| Разработчик, переключающийся между несколькими identity (work / personal / sandbox) | **local-vault** + несколько файлов | По vault на context, перебор без пересоздания keychain-записей. |
| Авто-агент в headless-окружении, где OS keychain требует разблокировки экрана | **local-vault** + passphrase из safe env-var | Нет UI-prompt'а, можно скриптовать unlock. |

> **Главный критерий**: local-vault — это для случаев, когда *нет* подходящего системного хранилища. Если OS keychain или 1Password доступны — берите их. Local-vault даёт переносимость и контроль над файлом ценой того, что бэкап и rotate ключей становятся вашей ответственностью.

## Формат файла

Файл живёт по умолчанию в `~/.devboy/secrets/local-vault.dvb`. Бинарный layout (см. `crates/devboy-vault-crypto/src/format.rs`):

```text
HEADER (53 байта, фиксированный)
  MAGIC       [4]   = b"DVB1"          // sanity-check + версия формата
  VERSION     [1]   = 0x01
  KDF_PARAMS [16]   = m_cost u32 LE,    // Argon2id memory cost (KiB)
                      t_cost u32 LE,    // iterations
                      p_cost u32 LE,    // parallelism
                      salt_len u32 LE
  SALT       [32]   = случайные 32 байта (KDF salt для passphrase envelope)

UNLOCK_ENVELOPES (length-prefixed TOML)
  [envelopes_len: u32 LE][envelopes_bytes...]
  // По одной envelope на способ разблокировки. Каждая envelope
  // независимо хранит зашифрованную копию vault-key.

ENTRIES_INDEX (length-prefixed TOML, метаданные)
  [entries_len: u32 LE][entries_bytes...]
  // path, description, retrieval_url, expires_at, pattern_id, …
  // Значений в индексе НЕТ — он читается без unlock.

AEAD_BLOBS (length-prefixed бинарный поток)
  [blobs_len: u64 LE][blob_bytes...]
  // Зашифрованные значения, по одному на entry. AEAD — XChaCha20-Poly1305,
  // AAD привязан к kind envelope, чтобы blob нельзя было перебросить
  // между несовместимыми ключами.
```

Магия `DVB1` фиксирована — её достаточно, чтобы `file local-vault.dvb` отличил vault от случайных байтов.

### Метаданные читаются без unlock

`description`, `retrieval_url`, `expires_at`, `rotation_method`, `pattern_id` лежат в открытом виде. Это умышленно — `secrets list`, `doctor` и discovery-флоу должны работать без PIN-prompt'а. Угрозовая модель: локальный процесс с правами на чтение файла уже видит то, что система отдаёт через `ls -la`. Шифровать метаданные — добавить prompt без реальной защиты.

## Способы разблокировки (envelopes)

Один vault может одновременно поддерживать несколько способов разблокировки. Каждый способ — отдельная envelope, которая независимо хранит зашифрованную копию `vault-key`. Любая из них разблокирует те же blobs.

### 1. Passphrase envelope (по умолчанию)

```text
unlock-key = HKDF(Argon2id(passphrase, salt, KDF_PARAMS), info="devboy-vault-envelope:passphrase:v1")
wrapped_key = XChaCha20-Poly1305(unlock-key).encrypt(vault-key, aad="devboy-vault-envelope:passphrase:v1")
```

Параметры Argon2id по умолчанию (`KdfParams::DEFAULT`): m=64 MiB, t=3, p=1, salt 32 B. На 2024-class hardware один разблок занимает ~250 ms. Менять параметры можно через `devboy secrets vault rekey` — старая salt сохраняется, чтобы все остальные envelope продолжали работать.

### 2. Keychain envelope (опционально)

OS keychain хранит fresh случайный 32-byte ключ; envelope wrapped_key — это `XChaCha20-Poly1305(keychain_key).encrypt(vault-key)`. Удобно: vault разблокируется одним fingerprint/PIN-prompt'ом из системы, без ввода passphrase.

Доступно только на macOS / Windows / Linux с gnome-keyring; другие платформы — passphrase + recovery phrase.

### 3. Recovery phrase envelope (BIP39)

12-слов BIP39, генерируется при создании vault. Envelope использует `HKDF(BIP39_seed)` без Argon2id (фраза сама по себе высокоэнтропийная). Recovery phrase — запасной ключ от vault на случай, когда:

- Passphrase забыта.
- Keychain-запись стёрта (новая ОС, переустановка).
- Файл vault мигрирует на машину без keychain.

Recovery phrase **никогда** не хранится автоматически. CLI печатает её один раз при создании vault, и пользователь сам решает, куда её положить (бумажный список, password manager, sealed envelope).

> **⚠ Без recovery phrase potentially permanent loss**: если passphrase забыта *и* keychain недоступен *и* recovery phrase не сохранена — vault не разблокировать. Дубликата ключа в системе нет, brute-force Argon2id с дефолтными параметрами займёт годы. Бэкапьте recovery phrase отдельно от vault-файла.

## Recovery: что делать, когда всё пошло не так

### Сценарий A — забыта passphrase

1. Если есть keychain envelope → разблок через keychain:

   ```bash
   devboy secrets agent start --use-keychain
   devboy secrets vault rekey --new-passphrase
   ```

   `rekey` пересоздаёт passphrase envelope с новой фразой; keychain envelope остаётся в силе.

2. Если keychain тоже недоступен → recovery phrase:

   ```bash
   devboy secrets vault unlock --recovery
   # Введите 12-слов BIP39
   devboy secrets vault rekey --new-passphrase
   ```

3. Если ни keychain, ни recovery phrase → восстанавливаете из бэкапа vault-файла, сделанного *до* потери ключа.

### Сценарий B — повреждён vault-файл

Магия не сходится / TOML не парсится / blob не декодируется:

1. Не паникуйте — не пишите ничего поверх повреждённого файла.
2. Скопируйте `~/.devboy/secrets/local-vault.dvb` в безопасное место (`local-vault.dvb.broken`).
3. Восстановите файл из последнего бэкапа.
4. Прогоните `devboy secrets validate`. Если зелёный — продолжайте работу.
5. Проанализируйте, что повредило файл: full disk, `kill -9` во время записи, ручное редактирование. Атомарная запись (`tmpfile + rename`) исключает повреждение от прерывания, но не от внешнего вмешательства.

### Сценарий C — мигрируете vault на новую машину

Поскольку vault — это один файл, миграция тривиальна:

```bash
# На старой машине
cp ~/.devboy/secrets/local-vault.dvb /tmp/vault-export.dvb

# Перенести через защищённый канал (не email, не Slack)
scp /tmp/vault-export.dvb new-host:/tmp/

# На новой машине
mkdir -p ~/.devboy/secrets
mv /tmp/vault-export.dvb ~/.devboy/secrets/local-vault.dvb
chmod 600 ~/.devboy/secrets/local-vault.dvb
devboy secrets vault unlock --passphrase   # если passphrase знаете
# или
devboy secrets vault unlock --recovery     # если только recovery
```

После разблокировки на новой машине добавьте новую keychain envelope:

```bash
devboy secrets vault add-envelope --kind keychain
```

## Резервное копирование

Vault — один файл, поэтому бэкап = регулярная копия + хранение recovery phrase отдельно.

### Минимальная политика (один разработчик)

1. Раз в сутки cron / `systemd --user` копирует `~/.devboy/secrets/local-vault.dvb` в зашифрованное хранилище (Time Machine, Backblaze, Restic-репозиторий с собственным паролем).
2. Recovery phrase — на бумаге в физически безопасном месте *И* в личном password manager (например, 1Password Personal). Никогда вместе с vault-файлом.
3. Раз в квартал — тестовое восстановление: разворачиваете бэкап на чистую машину/контейнер и пытаетесь разблокировать через recovery phrase.

### Что **не** делать

- ❌ Положить vault и recovery phrase в один и тот же бэкап-каталог. Если бэкап утечёт — утечёт всё.
- ❌ Хранить recovery phrase в plain-text файле в репозитории / на git-сервере. BIP39 — высокоэнтропийная, но публичная утечка превращает её в тривиальный unlock-key.
- ❌ Отправлять vault-файл через email / мессенджеры. AEAD защищает значения, но KDF salt в header утекает, что облегчает offline-перебор passphrase. Защищённые каналы (`scp`, `magic-wormhole`, S3 with KMS) — обязательно.
- ❌ Использовать слабую passphrase. Argon2id замедляет brute-force, но `password123` всё равно перебирается. Минимум 4 случайных слова через `diceware`/`xkpasswd` или 16+ символов из password manager.

### Хороший workflow

```bash
# Бэкап (через restic для прозрачности дедупликации)
restic -r s3:s3.amazonaws.com/<bucket>/<prefix> backup ~/.devboy/secrets/local-vault.dvb

# Раз в квартал — drill
restic -r s3:... restore latest --target /tmp/restore
DEVBOY_VAULT_PATH=/tmp/restore/local-vault.dvb devboy secrets vault unlock --recovery
DEVBOY_VAULT_PATH=/tmp/restore/local-vault.dvb devboy secrets list
rm -rf /tmp/restore
```

## Security boundary

- **AEAD выбор**: XChaCha20-Poly1305. Длинный nonce (24 байта) → можно генерировать случайно без счётчика. По производительности эквивалент ChaCha20.
- **AAD**: каждый ciphertext привязан к kind envelope (`passphrase` / `keychain` / `recovery`). Перенос blob между несовместимыми envelope'ами обнаруживается на этапе декрипта.
- **Zeroize**: `vault-key` и `unlock-key` обёрнуты в `secrecy::SecretBox`, который вызывает `zeroize` на drop. Передача через FFI к keychain backend'ам делается через minimal lifetime.
- **Lock posture**: после `idle_timeout` (по умолчанию 60 секунд без запросов) daemon zeroize'ит in-memory ключи. Разблок требуется заново.
- **Не защищает от**:
  - root/admin локального процесса с правами на чтение vault-файла + перехват envelope unlock-key из памяти daemon'а.
  - Side-channel-атак на CPU (Spectre-class).
  - Социальной инженерии (фишинг recovery phrase).

## См. также

- [`onboarding.md`](../onboarding.md) — первичная установка и настройка sources.
- [`agent-protocol.md`](../agent-protocol.md) — как AI-агенты взаимодействуют с vault через MCP.
- ADR-023 §3.1 (формат) и §3.2–§3.3 (envelopes) — формальная спецификация.
- `crates/devboy-vault-crypto/` — исходный код формата + AEAD + KDF.
