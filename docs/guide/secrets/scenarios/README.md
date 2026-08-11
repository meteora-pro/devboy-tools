# Secret-framework BDD scenarios

Eight Gherkin `.feature` files covering the user-facing behaviour of the secret framework — the setup-secrets onboarding, approve-on-use, catalog URL lifecycle, the agent trust boundary, the proposer noise-reduction series, the catalog → provision-dialog rendering contract, the local-vault unlock / create flow, and the first-run onboarding wizard (backend picker).

Каждый сценарий описывает конкретный наблюдаемый результат, который пользователь (разработчик или AI-агент) может проверить, выполнив описанные команды.

## Как сценарии связаны с кодом

Сценарии **не исполняются** раннером. Вместо этого каждый из них несёт тег `@covered-by:<имя_теста>`, а тест-гейт `crates/devboy-cli/tests/bdd_coverage.rs` в CI проверяет, что:

1. у каждого сценария есть хотя бы один `@covered-by:`;
2. каждый упомянутый тест реально существует в workspace;
3. имена сценариев внутри файла не повторяются.

Правило 2 — то, ради чего всё это. Удалили или переименовали тест — сценарий, который на нём держался, роняет CI, и автор обязан либо восстановить покрытие, либо признать, что поведение исчезло. До этого гейта спецификация могла расходиться с кодом сколько угодно и никто бы не заметил: спецификация, которая не может оказаться неправдой, — не спецификация.

### Почему не cucumber-rs

Рассматривали и отказались. Сценарии написаны на уровне абстракции, до которого честный step definition не дотягивается:

```gherkin
Then the build fails with a typed error because SecretString does not implement AgentSafeReply
```

```gherkin
When the user clicks "Deny" in the dialog
```

Такие шаги можно только сымитировать, а зелёный фейковый шаг **хуже** отсутствия раннера: спецификация выглядит проверенной, ничего не проверяя. Поведение, которое эти шаги описывают, уже покрыто — compile-fail-гейтами, юнит-тестами GUI-рендереров, процессными интеграционными тестами. Просто не из Gherkin.

### Признанные дыры: `@not-covered:`

Семь сценариев из 54 описывают поведение, которое сегодня не покрыто ничем: GUI-кнопка, чей результат ни один тест не читает; `is_first_run()`, который лезет в `dirs::config_dir()` и env напрямую и потому не запускается из теста; пиновые счётчики прополки, для которых нет фикстуры демо-проекта. Повесить на них «примерно подходящий» тест — значит заставить гейт **врать**, а это хуже самой дыры.

Такие сценарии несут `@not-covered:<причина>`, и точный их список приколочен константой `UNCOVERED` в `bdd_coverage.rs`. Список — **храповик**: сценарий без покрытия, которого нет в списке, роняет CI; и наоборот, если покрытие появилось, а запись в списке осталась — тоже роняет. Долг видно, и он не может вырасти случайно.

### Чего гейт НЕ гарантирует

Что названный тест проверяет именно то, что написано в сценарии. Гейт умеет проверить, что ссылка есть и куда-то ведёт; что она ведёт **туда**, проверяет только человек на ревью. Поэтому тег стоит прямо над сценарием — чтобы ревьюер читал оба сразу.

Многие связки покрывают сценарий **частично**: у сценария пять `Then`, а тест проверяет три. Это нормально и намеренно — гейт держит связь живой, а не измеряет полноту.

## Files

| File | Covers |
|---|---|
| [`onboarding-wizard.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/onboarding-wizard.feature) | `devboy secrets setup --scan-only / --write-manifest / --resume` — the happy path, the resume contract, the catalog-driven proposer accuracy on a real project. |
| [`approve-on-use.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/approve-on-use.feature) | `approve_on_use = never / session / per-call` policy, dialog flow, `SessionApprovalCache` semantics, threat-model alignment (agent cannot escalate a deny). |
| [`catalog-url-source.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/catalog-url-source.feature) | `catalog add-url / status / refresh / forget / pin` — the full TOFU recovery + pin promotion flow. |
| [`agent-trust-boundary.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/agent-trust-boundary.feature) | "Agent never sees the value" enforced by `AgentSafeReply` marker + CI grep gate + negative test; covers every `secrets_*` MCP tool. |
| [`proposer-noise-reduction.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/proposer-noise-reduction.feature) | The five-step skip-list expansion (P1-P5) plus the catalog-driven precision (S2 + bundled catalogs) that took the proposer from 236 to 161 paths on the canonical demo project. |
| [`ui-catalog-rendering.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/ui-catalog-rendering.feature) | The provision dialog binds to the active token catalog (description / numbered steps / notes / console URL), with manifest-only fallback when no catalog matches the path. Covers both the egui and ratatui renderers (U-series). |
| [`vault-unlock.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/vault-unlock.feature) | The local-vault unlock / create flow in `secrets ui` — env-passphrase fast path, modal unlock prompt for an existing `.dvb`, wrong-passphrase handling, the keychain escape hatch, first-run create-vault with the recovery-phrase gate, and live backend switching (V-series). |
| [`onboarding.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/onboarding.feature) | The first-run onboarding wizard — backend picker (keychain / local-vault / HCP Vault, combinations allowed), per-backend sub-forms, the `access` mode for HCP Vault, `sources.toml` write, primary-backend resolution, and the P11 (multi-source routing) / P15 (write path) deferral boundaries (W-series). |

## Why Gherkin

The BDD shape forces every behaviour into a `Given / When / Then` triple, which is invaluable when the surface spans CLI, MCP wire-format, GUI dialog, and a daemon over a UNIX socket. A reviewer who reads the `.feature` files can sanity-check that:

- the documented user actions cover every "happy path" the implementation pretends to support,
- the failure modes have explicit scenarios and aren't just "the test asserts an error",
- the trust boundary is stated as a positive contract ("agent receives only the verdict") rather than left implicit.

## When to add a new scenario

Add a `.feature` block any time:

- a CLI command grows a new flag whose semantics differ from the default (e.g. `add-url --pin` vs `add-url` alone),
- the MCP wire format gains a new field that the agent should understand,
- a new policy value lands on the manifest schema (e.g. extending `approve_on_use` with a `Project` or `Org` scope in a future epic).

Keep scenarios concrete — name actual env vars, paths, error reasons. The Examples table is the right place for breadth (P1-P5 outlines).

### Обязательный тег покрытия

Новый сценарий без `@covered-by:` не пройдёт CI. Тег ставится строкой выше заголовка:

```gherkin
  @covered-by:legacy_env_names_still_resolve_in_ci_mode
  Scenario: An ADR-005 pipeline keeps working after the default flip
    Given the pipeline exports DEVBOY_GITLAB_TOKEN
    ...
```

Тегов может быть несколько через пробел. Порядок такой: **сначала тест, потом сценарий.** Если покрывающего теста ещё нет — это не повод писать сценарий «на будущее»: непокрытый сценарий обещает то, чего набор тестов не выполняет.
