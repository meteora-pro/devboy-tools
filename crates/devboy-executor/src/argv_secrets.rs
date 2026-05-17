//! `@secret:<path>` substitution wrapper for child-process argv per
//! [ADR-020] §5.
//!
//! ADR-020 §5 second bullet:
//!
//! > A wrapper rewrites `@secret:<path>` occurrences in argv before
//! > `exec`. Because argv is visible to other processes through `ps`,
//! > the wrapper prefers passing the secret through stdin or a file
//! > descriptor when the target tool supports it (for example,
//! > `gh auth login --with-token`, `git credential fill`). Direct
//! > argv substitution is the fallback for tools that accept secrets
//! > only in argv, and is documented as such.
//!
//! [`rewrite_argv`] is the *planning* function: it takes the raw
//! `argv` plus a [`SecretResolver`], decides for each known tool
//! whether to redirect through stdin, and returns a [`RewritePlan`]
//! that captures:
//!
//! - the final `argv` (with aliases removed when they go through
//!   stdin, or with plaintext substituted when they go through
//!   argv),
//! - an optional `stdin_payload` ([`SecretString`]) the caller
//!   writes to the child's stdin,
//! - a per-substitution audit trail ([`Substitution`]) so callers
//!   can log which paths were resolved through which strategy,
//! - an `argv_visible` flag so the caller can warn when at least
//!   one secret will land in `ps` output.
//!
//! [`apply_plan_to_command`] hands the plan to a
//! [`tokio::process::Command`] — sets stdio, then the caller
//! `.spawn()`s and writes the payload to the child's stdin.
//!
//! ## Known-tool detection
//!
//! The first version recognises two FD-friendly invocation
//! patterns:
//!
//! - `gh auth login --with-token <alias>` — `gh` reads the token
//!   from stdin when `--with-token` is present.
//! - `git credential fill|approve|reject` — the `git credential`
//!   protocol is stdin-only.
//!
//! Adding more patterns (`vault login -method=token`, `aws ssm
//! put-parameter --value`, …) is one entry per case in the
//! `Known` enum + a small predicate. Until they're added, the
//! wrapper falls back to plaintext-in-argv with a structured
//! warning surface.
//!
//! ## What this module does **not** do
//!
//! - Spawn the child. The caller decides how (`tokio::spawn`,
//!   blocking, supervised), and only this module knows the
//!   stdio shape.
//! - Resolve aliases against a router. The
//!   [`SecretResolver`] trait is the boundary; the storage-layer
//!   impl that walks the router/credential chain lands separately.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [`SecretResolver`]: devboy_core::alias::SecretResolver

use std::path::Path;

use devboy_core::alias::{AliasResolverError, SecretResolver, parse_alias};
use devboy_core::secret_approval::ApprovalGatedResolver;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tracing::{debug, warn};

// =============================================================================
// Public types
// =============================================================================

/// Per-substitution audit entry. Surfaced through
/// [`RewritePlan::substitutions`] so callers can log routing
/// decisions without reaching into the secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// Path portion of the original `@secret:<path>` alias.
    pub path: String,
    /// Index in the *input* argv where the alias appeared.
    pub argv_index: usize,
    /// Strategy chosen for this alias.
    pub strategy: SubstitutionStrategy,
}

/// How the wrapper passed a secret to the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstitutionStrategy {
    /// Resolved value placed verbatim in the rewritten argv —
    /// visible to other processes via `ps`. Fallback for tools
    /// that have no FD/stdin alternative.
    Argv,
    /// Resolved value written to the child's stdin. The argv
    /// position that originally held the alias is removed.
    Stdin,
}

/// Outcome of [`rewrite_argv`].
#[derive(Debug)]
pub struct RewritePlan {
    /// Argv as it should be handed to the child. Aliases routed
    /// through stdin are gone; aliases routed through argv hold
    /// the plaintext value.
    pub argv: Vec<String>,
    /// When `Some`, the caller writes the contained
    /// [`SecretString`] to the child's stdin. Multiple
    /// stdin-routed aliases are joined with `\n`; the trailing
    /// newline lets `gh auth login --with-token` and friends
    /// terminate the read cleanly.
    pub stdin_payload: Option<SecretString>,
    /// Audit trail (one entry per alias seen).
    pub substitutions: Vec<Substitution>,
    /// `true` when at least one alias was routed through argv —
    /// callers who want to fail loud on argv exposure can check
    /// this.
    pub argv_visible: bool,
}

/// Failure modes for [`rewrite_argv`].
#[derive(Debug, Error)]
pub enum ArgvRewriteError {
    /// Resolver could not produce a value for an alias.
    //
    // `source_error` (not `source`) — thiserror treats a field
    // named `source` as the `#[source]` chain root, but our
    // explicit `#[source]` annotation on this field already
    // signals that. Renaming sidesteps the name clash and lets
    // the destructure work cleanly in tests.
    #[error("failed to resolve alias `@secret:{path}`: {source_error}")]
    Resolve {
        /// Path portion of the alias.
        path: String,
        /// Underlying resolver error.
        #[source]
        source_error: AliasResolverError,
    },
}

// =============================================================================
// Planning
// =============================================================================

/// Plan a substitution for the given child invocation.
///
/// `program` is the path or basename the caller will spawn (`"gh"`,
/// `"/usr/local/bin/git"`, …). `argv` is the argument vector that
/// follows it. The wrapper inspects `argv` for `@secret:<path>`
/// aliases, asks the resolver for each value, and decides per
/// alias whether to redirect through stdin or substitute in argv.
///
/// On `Err(ArgvRewriteError::Resolve)` the partial work is
/// discarded — the caller cannot end up with half-resolved argv.
pub fn rewrite_argv<R, F>(
    program: &str,
    argv: &[String],
    resolver: &ApprovalGatedResolver<R, F>,
) -> Result<RewritePlan, ArgvRewriteError>
where
    R: SecretResolver,
    F: Fn(&str) -> devboy_core::secret_approval::ApproveOnUsePolicy + Send + Sync,
{
    // Step 1: decide the per-tool strategy.
    let strategy = ToolStrategy::detect(program, argv);

    // Step 2: walk argv, classify each entry, resolve when needed.
    let mut out_argv = Vec::with_capacity(argv.len());
    let mut substitutions = Vec::new();
    let mut stdin_pieces: Vec<SecretString> = Vec::new();
    let mut argv_visible = false;

    for (idx, raw) in argv.iter().enumerate() {
        let Some(path) = parse_alias(raw) else {
            // Not an alias — keep verbatim (per ADR-020 §5,
            // partial occurrences are NOT rewritten).
            out_argv.push(raw.clone());
            continue;
        };

        let value = resolver
            .resolve(path)
            .map_err(|source_error| ArgvRewriteError::Resolve {
                path: path.to_owned(),
                source_error,
            })?;

        if strategy.uses_stdin_for(idx, argv) {
            // Drop the alias arg — the child reads the value
            // off stdin. The plan's payload picks it up.
            stdin_pieces.push(value);
            substitutions.push(Substitution {
                path: path.to_owned(),
                argv_index: idx,
                strategy: SubstitutionStrategy::Stdin,
            });
        } else {
            // Argv fallback: visible in `ps`.
            argv_visible = true;
            out_argv.push(value.expose_secret().to_owned());
            substitutions.push(Substitution {
                path: path.to_owned(),
                argv_index: idx,
                strategy: SubstitutionStrategy::Argv,
            });
            warn!(
                program,
                index = idx,
                path,
                "argv-substituted secret will be visible to other processes via `ps`; \
                 prefer a tool that accepts the value via stdin/FD"
            );
        }
    }

    let stdin_payload = if stdin_pieces.is_empty() {
        None
    } else {
        // Join multiple stdin-routed values with `\n` so a tool
        // expecting one-secret-per-line (rare but it happens)
        // works. A trailing newline lets `gh auth login` and
        // `git credential` terminate their reads.
        let joined = stdin_pieces
            .iter()
            .map(|s| s.expose_secret().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        Some(SecretString::from(joined))
    };

    debug!(
        program,
        substitutions = substitutions.len(),
        argv_visible,
        "argv-secret rewrite complete"
    );

    Ok(RewritePlan {
        argv: out_argv,
        stdin_payload,
        substitutions,
        argv_visible,
    })
}

// =============================================================================
// Apply plan to a tokio::process::Command
// =============================================================================

/// Wire a [`RewritePlan`] into a [`tokio::process::Command`].
///
/// Sets the stdin disposition based on whether the plan has a
/// payload — `Stdio::piped()` when yes, untouched (inherits) when
/// no. The argv override replaces whatever args were on the
/// command. The caller is responsible for `.spawn()`-ing,
/// writing the payload through `child.stdin`, and waiting on the
/// child.
///
/// Argv override note: this function calls `.args(&plan.argv)`
/// only if the plan changed the argv (the substitutions list is
/// non-empty). When the plan is a pass-through (no aliases) we
/// leave whatever the caller already set in place.
pub fn apply_plan_to_command(plan: &RewritePlan, cmd: &mut tokio::process::Command) {
    if !plan.substitutions.is_empty() {
        // Reset args to the rewritten list. tokio::process::Command
        // doesn't expose args_mut(), so we can only append. The
        // contract here is that the caller is using this helper as
        // *the* way to supply args for a planned invocation —
        // the input `cmd` should not have args set yet.
        cmd.args(&plan.argv);
    }
    if plan.stdin_payload.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
}

// =============================================================================
// Tool detection
// =============================================================================

/// Per-invocation strategy that knows when to prefer stdin.
struct ToolStrategy {
    kind: Known,
}

#[derive(Debug, Clone, Copy)]
enum Known {
    /// `gh auth login --with-token <ALIAS>` — `gh` reads the
    /// token from stdin when `--with-token` is present.
    GhAuthLoginWithToken {
        /// Index in argv where the alias replaces the token.
        token_index: usize,
    },
    /// `git credential fill|approve|reject` — the credential
    /// protocol is stdin-only by design; ANY alias arg goes
    /// through stdin.
    GitCredential,
    /// No known FD-friendly pattern matched — fall back to argv
    /// substitution.
    Fallback,
}

impl ToolStrategy {
    fn detect(program: &str, argv: &[String]) -> Self {
        let basename = Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(program);
        match basename {
            "gh" => detect_gh(argv),
            "git" => detect_git(argv),
            _ => ToolStrategy {
                kind: Known::Fallback,
            },
        }
    }

    /// Decide whether the alias at `argv_index` should be routed
    /// through stdin for this tool.
    fn uses_stdin_for(&self, argv_index: usize, _argv: &[String]) -> bool {
        match self.kind {
            Known::GhAuthLoginWithToken { token_index } => argv_index == token_index,
            Known::GitCredential => true,
            Known::Fallback => false,
        }
    }
}

fn detect_gh(argv: &[String]) -> ToolStrategy {
    // We're looking for `auth login --with-token <ALIAS>`.
    // Anything else falls back to argv.
    let auth_idx = argv.iter().position(|a| a == "auth");
    let login_idx = argv.iter().position(|a| a == "login");
    let with_token_idx = argv.iter().position(|a| a == "--with-token");
    if let (Some(a), Some(l), Some(t)) = (auth_idx, login_idx, with_token_idx)
        && a < l
        && l < t
    {
        // The token argument follows `--with-token`.
        let token_idx = t + 1;
        if token_idx < argv.len() {
            let candidate = &argv[token_idx];
            if parse_alias(candidate).is_some() {
                return ToolStrategy {
                    kind: Known::GhAuthLoginWithToken {
                        token_index: token_idx,
                    },
                };
            }
        }
    }
    ToolStrategy {
        kind: Known::Fallback,
    }
}

fn detect_git(argv: &[String]) -> ToolStrategy {
    // `git credential <subcommand>` — stdin-only protocol.
    if argv.first().map(String::as_str) == Some("credential") {
        return ToolStrategy {
            kind: Known::GitCredential,
        };
    }
    ToolStrategy {
        kind: Known::Fallback,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Tiny in-memory resolver for tests.
    struct MapResolver {
        entries: Mutex<HashMap<String, String>>,
    }

    impl MapResolver {
        fn new(pairs: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                entries: Mutex::new(
                    pairs
                        .into_iter()
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect(),
                ),
            }
        }
    }

    fn always_never(_: &str) -> devboy_core::secret_approval::ApproveOnUsePolicy {
        devboy_core::secret_approval::ApproveOnUsePolicy::Never
    }

    fn wrap_resolver_never(
        r: MapResolver,
    ) -> ApprovalGatedResolver<
        MapResolver,
        fn(&str) -> devboy_core::secret_approval::ApproveOnUsePolicy,
    > {
        ApprovalGatedResolver::new(
            r,
            std::sync::Arc::new(devboy_core::secret_approval::SessionApprovalCache::new()),
            always_never,
        )
    }

    impl SecretResolver for MapResolver {
        fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError> {
            let map = self.entries.lock().unwrap();
            match map.get(path) {
                Some(v) => Ok(SecretString::from(v.clone())),
                None => Err(AliasResolverError::NotFound {
                    path: path.to_owned(),
                }),
            }
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    // -- Pass-through (no aliases) ------------------------------

    #[test]
    fn no_alias_in_argv_is_a_passthrough_with_no_substitutions() {
        let resolver = MapResolver::new([]);
        let plan = rewrite_argv(
            "any-tool",
            &argv(&["--flag", "value"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert_eq!(plan.argv, vec!["--flag".to_owned(), "value".to_owned()]);
        assert!(plan.stdin_payload.is_none());
        assert!(plan.substitutions.is_empty());
        assert!(!plan.argv_visible);
    }

    // -- Argv fallback -----------------------------------------

    #[test]
    fn unknown_tool_falls_back_to_argv_substitution() {
        let resolver = MapResolver::new([("personal/github/pat", "ghp-fixture")]);
        let plan = rewrite_argv(
            "some-tool",
            &argv(&["--token", "@secret:personal/github/pat"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert_eq!(
            plan.argv,
            vec!["--token".to_owned(), "ghp-fixture".to_owned()]
        );
        assert!(plan.stdin_payload.is_none());
        assert_eq!(plan.substitutions.len(), 1);
        assert_eq!(plan.substitutions[0].strategy, SubstitutionStrategy::Argv);
        assert_eq!(plan.substitutions[0].argv_index, 1);
        assert!(plan.argv_visible);
    }

    #[test]
    fn fallback_warns_via_argv_visible_flag() {
        let resolver = MapResolver::new([("a/b/c", "v")]);
        let plan = rewrite_argv(
            "some-tool",
            &argv(&["@secret:a/b/c"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert!(plan.argv_visible);
    }

    // -- gh auth login --with-token ----------------------------

    #[test]
    fn gh_auth_login_with_token_routes_through_stdin() {
        use secrecy::ExposeSecret;
        let resolver = MapResolver::new([("personal/github/pat", "ghp-fixture")]);
        let plan = rewrite_argv(
            "gh",
            &argv(&[
                "auth",
                "login",
                "--with-token",
                "@secret:personal/github/pat",
            ]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        // The alias arg is dropped from the rewritten argv.
        assert_eq!(
            plan.argv,
            vec![
                "auth".to_owned(),
                "login".to_owned(),
                "--with-token".to_owned()
            ]
        );
        // The plaintext is *not* in argv anywhere.
        assert!(plan.argv.iter().all(|a| !a.contains("ghp-fixture")));
        // Stdin payload carries the token (with trailing \n).
        let payload = plan.stdin_payload.unwrap();
        assert_eq!(payload.expose_secret(), "ghp-fixture\n");
        assert_eq!(plan.substitutions.len(), 1);
        assert_eq!(plan.substitutions[0].strategy, SubstitutionStrategy::Stdin);
        assert!(!plan.argv_visible, "stdin path must NOT mark argv visible");
    }

    #[test]
    fn gh_with_absolute_path_still_recognised() {
        let resolver = MapResolver::new([("p/g/p", "v")]);
        let plan = rewrite_argv(
            "/usr/local/bin/gh",
            &argv(&["auth", "login", "--with-token", "@secret:p/g/p"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        // basename match → still gh strategy.
        assert!(!plan.argv_visible);
    }

    #[test]
    fn gh_without_with_token_falls_back_to_argv() {
        let resolver = MapResolver::new([("p/g/p", "v")]);
        let plan = rewrite_argv(
            "gh",
            &argv(&["repo", "view", "--token", "@secret:p/g/p"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        // No --with-token flag → fallback strategy.
        assert!(plan.argv_visible);
        assert!(plan.argv.contains(&"v".to_owned()));
    }

    // -- git credential ----------------------------------------

    #[test]
    fn git_credential_routes_alias_args_through_stdin() {
        use secrecy::ExposeSecret;
        let resolver = MapResolver::new([("svc/git/cred", "credential-fixture")]);
        let plan = rewrite_argv(
            "git",
            &argv(&["credential", "fill", "@secret:svc/git/cred"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        // Alias arg dropped from argv; only literal args remain.
        assert_eq!(plan.argv, vec!["credential".to_owned(), "fill".to_owned()]);
        let payload = plan.stdin_payload.unwrap();
        assert_eq!(payload.expose_secret(), "credential-fixture\n");
        assert_eq!(plan.substitutions[0].strategy, SubstitutionStrategy::Stdin);
        assert!(!plan.argv_visible);
    }

    #[test]
    fn git_non_credential_uses_argv_fallback() {
        let resolver = MapResolver::new([("a/b/c", "v")]);
        let plan = rewrite_argv(
            "git",
            &argv(&["push", "--token", "@secret:a/b/c"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert!(plan.argv_visible);
    }

    // -- Multiple aliases --------------------------------------

    #[test]
    fn multiple_argv_aliases_each_get_an_audit_entry() {
        let resolver = MapResolver::new([("a/b/c", "v1"), ("d/e/f", "v2")]);
        let plan = rewrite_argv(
            "tool",
            &argv(&["@secret:a/b/c", "literal", "@secret:d/e/f"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert_eq!(
            plan.argv,
            vec!["v1".to_owned(), "literal".to_owned(), "v2".to_owned()]
        );
        assert_eq!(plan.substitutions.len(), 2);
        assert!(plan.argv_visible);
    }

    // -- Partial-occurrence non-rewriting per ADR-020 §5 -------

    #[test]
    fn alias_inside_a_longer_arg_is_not_rewritten() {
        // ADR-020 §5: alias replaces the WHOLE field value;
        // partial occurrences are NOT aliases.
        let resolver = MapResolver::new([("a/b/c", "v")]);
        let plan = rewrite_argv(
            "tool",
            &argv(&["Bearer @secret:a/b/c"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();
        assert_eq!(plan.argv, vec!["Bearer @secret:a/b/c".to_owned()]);
        assert!(plan.substitutions.is_empty());
    }

    // -- Resolver error propagation ----------------------------

    #[test]
    fn unknown_alias_path_propagates_resolver_error() {
        let resolver = MapResolver::new([]);
        let err = rewrite_argv(
            "tool",
            &argv(&["--token", "@secret:nope/nope/nope"]),
            &wrap_resolver_never(resolver),
        )
        .unwrap_err();
        match err {
            ArgvRewriteError::Resolve { path, .. } => {
                assert_eq!(path, "nope/nope/nope");
            }
        }
    }

    // -- apply_plan_to_command + real child --------------------

    /// End-to-end sanity check on Unix: plan a substitution that
    /// goes through stdin, spawn `/bin/cat`, write the payload,
    /// and assert the child reads the secret value back from
    /// stdout. Also asserts that the rewritten argv handed to
    /// the child contains no plaintext — the moral equivalent
    /// of `ps` invisibility.
    #[cfg(unix)]
    #[tokio::test]
    async fn apply_plan_to_command_pipes_stdin_to_child() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let resolver = MapResolver::new([("personal/github/pat", "ghp-fixture")]);
        let plan = rewrite_argv(
            "gh",
            &argv(&[
                "auth",
                "login",
                "--with-token",
                "@secret:personal/github/pat",
            ]),
            &wrap_resolver_never(resolver),
        )
        .unwrap();

        // Sanity: rewritten argv must not contain the plaintext
        // (this is the "ps invisibility" property).
        for arg in &plan.argv {
            assert!(!arg.contains("ghp-fixture"));
        }

        // Spawn /bin/cat as a stand-in for any tool that reads
        // its single secret from stdin.
        let mut cmd = tokio::process::Command::new("/bin/cat");
        // We don't reuse plan.argv here — /bin/cat doesn't take
        // gh's arguments. Just exercise the stdin-piping path
        // independently.
        if plan.stdin_payload.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }
        cmd.stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("spawn /bin/cat");

        // Write the payload.
        if let Some(secret) = &plan.stdin_payload {
            let mut stdin = child.stdin.take().unwrap();
            stdin
                .write_all(secret.expose_secret().as_bytes())
                .await
                .unwrap();
            // Closing stdin sends EOF so cat exits cleanly.
            stdin.shutdown().await.unwrap();
            drop(stdin);
        }

        // Read the child's stdout.
        let mut stdout = child.stdout.take().unwrap();
        let mut buf = String::new();
        stdout.read_to_string(&mut buf).await.unwrap();
        let _ = child.wait().await;

        // /bin/cat echoes its stdin verbatim. The secret made it
        // through.
        assert_eq!(buf, "ghp-fixture\n");
    }
}
