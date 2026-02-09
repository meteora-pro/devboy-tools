import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import styles from './RepoConfigExtractor.module.css';

const PLACEHOLDER_TOKEN = '<your-token>';
const COPY_TIMEOUT_MS = 2000;
const GITHUB_REPO_REGEX = /github\.com\/([^\/]+)\/([^\/]+?)(?:\.git)?(?:\/.*)?$/;

export default function RepoConfigExtractor() {
  const [url, setUrl] = useState('');
  const [token, setToken] = useState('');
  const [owner, setOwner] = useState('');
  const [repo, setRepo] = useState('');
  const [copied, setCopied] = useState(false);
  const [showTokenHelp, setShowTokenHelp] = useState(false);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, []);

  const extractRepoInfo = useCallback((input: string) => {
    const match = input.match(GITHUB_REPO_REGEX);
    if (match) {
      setOwner(match[1]);
      setRepo(match[2]);
    } else {
      setOwner('');
      setRepo('');
    }
  }, []);

  const handleUrlChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value;
    setUrl(value);
    extractRepoInfo(value);
    setCopied(false);
  };

  const handleTokenChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setToken(e.target.value);
    setCopied(false);
  };

  const tokenDisplay = token || PLACEHOLDER_TOKEN;
  const commands = useMemo(() => {
    if (!owner || !repo) return '';
    return `devboy config set github.owner ${owner} \\\ndevboy config set github.repo ${repo} \\\ndevboy config set-secret github.token ${tokenDisplay}`;
  }, [owner, repo, tokenDisplay]);

  const copyToClipboard = useCallback(async () => {
    if (!commands) return;

    try {
      await navigator.clipboard.writeText(commands);
      setCopied(true);

      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }

      timeoutRef.current = setTimeout(() => {
        setCopied(false);
      }, COPY_TIMEOUT_MS);
    } catch {
      console.error('Failed to copy to clipboard');
    }
  }, [commands]);

  const urlInputId = 'repo-url';
  const tokenInputId = 'github-token';

  return (
    <div className={styles['config-generator']}>
      <h4 className={styles['config-generator__title']}>Quick Config Generator</h4>

      <div className={styles['config-generator__field']}>
        <label htmlFor={urlInputId} className={styles['config-generator__label']}>
          Repository URL
        </label>
        <input
          id={urlInputId}
          type="text"
          value={url}
          onChange={handleUrlChange}
          placeholder="https://github.com/owner/repo"
          className={styles['config-generator__input']}
        />
      </div>

      <div className={styles['config-generator__field']}>
        <label htmlFor={tokenInputId} className={styles['config-generator__label']}>
          GitHub Token
        </label>
        <input
          id={tokenInputId}
          type="password"
          value={token}
          onChange={handleTokenChange}
          placeholder="your token"
          className={styles['config-generator__input']}
        />
        <button
          type="button"
          onClick={() => setShowTokenHelp(!showTokenHelp)}
          className={styles['config-generator__link']}
          aria-expanded={showTokenHelp}
          aria-controls="token-help"
        >
          {showTokenHelp ? 'Hide instructions' : 'How to get a token?'}
        </button>

        {showTokenHelp && (
          <div id="token-help" className={styles['config-generator__help']}>
            <p className={styles['config-generator__help-title']}>
              <strong>Steps to create a GitHub token:</strong>
            </p>
            <ol className={styles['config-generator__help-list']}>
              <li>
                Go to GitHub → Settings → Developer settings → Personal access
                tokens → Tokens (classic)
              </li>
              <li>
                Click <strong>Generate new token (classic)</strong>
              </li>
              <li>Give it a name (e.g., &quot;DevBoy Tools&quot;)</li>
              <li>
                Select these scopes:
                <ul className={styles['config-generator__help-sublist']}>
                  <li>
                    <code>repo</code> — Full repository access
                  </li>
                  <li>
                    <code>read:user</code> — Read user information
                  </li>
                </ul>
              </li>
              <li>
                Click <strong>Generate token</strong> and copy it immediately
              </li>
            </ol>
          </div>
        )}
      </div>

      {owner && repo && (
        <section className={styles['config-generator__result']}>
          <div className={styles['config-generator__info']}>
            <span>
              <strong>Owner:</strong> {owner}
            </span>
            <span>
              <strong>Repo:</strong> {repo}
            </span>
          </div>

          <div className={styles['config-generator__code-block']}>
            <pre className={styles['config-generator__code']}>{commands}</pre>
            <button
              type="button"
              onClick={copyToClipboard}
              className={
                copied
                  ? `${styles['config-generator__copy-btn']} ${styles['config-generator__copy-btn--copied']}`
                  : styles['config-generator__copy-btn']
              }
              aria-label={copied ? 'Copied to clipboard' : 'Copy to clipboard'}
            >
              {copied ? 'Copied!' : 'Copy'}
            </button>
          </div>
        </section>
      )}
    </div>
  );
}
