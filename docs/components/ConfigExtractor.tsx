import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import styles from './ConfigExtractor.module.css';

const PLACEHOLDER_TOKEN = '<your-token>';
const COPY_TIMEOUT_MS = 2000;

type ExtractedField = {
  label: string;
  value: string;
};

type TokenScope = {
  code: string;
  description: string;
};

type TokenHelpStep = {
  text: string;
  bold?: string;
  scopes?: TokenScope[];
};

type ExtraField = {
  key: string;
  label: string;
  placeholder: string;
  hint?: string;
};

type ProviderConfig = {
  regex: RegExp;
  urlPlaceholder: string;
  tokenLabel: string;
  urlLabel: string;
  extractInfo: (match: RegExpMatchArray) => ExtractedField[];
  extraFields?: ExtraField[];
  autoFillExtra?: (match: RegExpMatchArray) => Record<string, string>;
  commands: (info: ExtractedField[], extra: Record<string, string>, token: string) => string;
  tokenHelp: {
    title: string;
    steps: TokenHelpStep[];
  };
};

const PROVIDERS: Record<string, ProviderConfig> = {
  github: {
    regex: /github\.com\/([^\/]+)\/([^\/]+?)(?:\.git)?(?:\/.*)?$/,
    urlPlaceholder: 'https://github.com/owner/repo',
    tokenLabel: 'GitHub Token',
    urlLabel: 'Repository URL',
    extractInfo: (match) => [
      { label: 'Owner', value: match[1] },
      { label: 'Repo', value: match[2] },
    ],
    commands: (info, _extra, token) =>
      `devboy config set github.owner ${info[0].value} \\\ndevboy config set github.repo ${info[1].value} \\\ndevboy config set-secret github.token ${token}`,
    tokenHelp: {
      title: 'Steps to create a GitHub token:',
      steps: [
        { text: 'Go to GitHub → Settings → Developer settings → Personal access tokens → Tokens (classic)' },
        { text: '', bold: 'Generate new token (classic)' },
        { text: 'Give it a name (e.g., "DevBoy Tools")' },
        {
          text: 'Select these scopes:',
          scopes: [
            { code: 'repo', description: 'Full repository access' },
            { code: 'read:user', description: 'Read user information' },
          ],
        },
        { text: '', bold: 'Generate token' },
      ],
    },
  },
  gitlab: {
    regex: /^(https?:\/\/[^\/]+)\/(.+?)(?:\.git)?(?:\/-\/.*)?$/,
    urlPlaceholder: 'https://gitlab.com/group/project',
    tokenLabel: 'GitLab Token',
    urlLabel: 'Project URL',
    extractInfo: (match) => [
      { label: 'Instance', value: match[1] },
    ],
    extraFields: [
      {
        key: 'projectId',
        label: 'Project ID',
        placeholder: '12345 or group/project',
        hint: 'Go to your project\'s main page — the numeric ID is shown below the project name. You can also use the full path (e.g., group/subgroup/project).',
      },
    ],
    autoFillExtra: (match) => ({ projectId: match[2] }),
    commands: (info, extra, token) =>
      `devboy config set gitlab.url ${info[0].value} \\\ndevboy config set gitlab.project_id ${extra.projectId || '<project-id>'} \\\ndevboy config set-secret gitlab.token ${token}`,
    tokenHelp: {
      title: 'Steps to create a GitLab Personal Access Token:',
      steps: [
        { text: 'Go to GitLab → User Settings → Access Tokens' },
        { text: '', bold: 'Add new token' },
        { text: 'Give it a name (e.g., "DevBoy Tools")' },
        {
          text: 'Select these scopes:',
          scopes: [
            { code: 'api', description: 'Full API access (issues, MRs, comments, diffs)' },
            { code: 'read_user', description: 'Read user information' },
          ],
        },
        { text: '', bold: 'Create personal access token' },
      ],
    },
  },
  clickup: {
    regex: /app\.clickup\.com\/(\d+)\/v\/(?:li\/(\d+)|l\/[a-z\d]+-(\d+)(?:-\d+)?)/i,
    urlPlaceholder: 'https://app.clickup.com/12345678/v/l/6-901234567890-1',
    tokenLabel: 'ClickUp Token',
    urlLabel: 'List URL',
    extractInfo: (match) => [
      { label: 'Team ID', value: match[1] },
      { label: 'List ID', value: match[2] || match[3] },
    ],
    commands: (info, _extra, token) =>
      `devboy config set clickup.list_id ${info[1].value} \\\ndevboy config set clickup.team_id ${info[0].value} \\\ndevboy config set-secret clickup.token ${token}`,
    tokenHelp: {
      title: 'Steps to create a ClickUp Personal API Token:',
      steps: [
        { text: 'Go to ClickUp → Settings → Apps' },
        { text: 'Find the', bold: 'API Token section' },
        { text: '', bold: 'Generate' },
        { text: 'Copy the token immediately — it won\'t be shown again' },
      ],
    },
  },
};

type ConfigExtractorProps = {
  provider: 'github' | 'gitlab' | 'clickup';
};

export default function ConfigExtractor({ provider }: ConfigExtractorProps) {
  const config = PROVIDERS[provider];
  const [url, setUrl] = useState('');
  const [token, setToken] = useState('');
  const [infoFields, setInfoFields] = useState<ExtractedField[]>([]);
  const [extraValues, setExtraValues] = useState<Record<string, string>>({});
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

  const extractInfo = useCallback(
    (input: string) => {
      const match = input.match(config.regex);
      if (match) {
        setInfoFields(config.extractInfo(match));
        if (config.autoFillExtra) {
          setExtraValues((prev) => {
            const autoFilled = config.autoFillExtra!(match);
            const merged = { ...prev };
            for (const [key, value] of Object.entries(autoFilled)) {
              if (!merged[key]) {
                merged[key] = value;
              }
            }
            return merged;
          });
        }
      } else {
        setInfoFields([]);
      }
    },
    [config],
  );

  const handleUrlChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value;
    setUrl(value);
    extractInfo(value);
    setCopied(false);
  };

  const handleTokenChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setToken(e.target.value);
    setCopied(false);
  };

  const handleExtraChange = (key: string, value: string) => {
    setExtraValues((prev) => ({ ...prev, [key]: value }));
    setCopied(false);
  };

  const tokenDisplay = token || PLACEHOLDER_TOKEN;
  const hasInfo = infoFields.length > 0;

  const commands = useMemo(() => {
    if (!hasInfo) return '';
    return config.commands(infoFields, extraValues, tokenDisplay);
  }, [config, infoFields, extraValues, hasInfo, tokenDisplay]);

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

  const urlInputId = `${provider}-url`;
  const tokenInputId = `${provider}-token`;
  const helpId = `${provider}-token-help`;

  return (
    <div className={styles['config-generator']}>
      <h4 className={styles['config-generator__title']}>Quick Config Generator</h4>

      <div className={styles['config-generator__field']}>
        <label htmlFor={urlInputId} className={styles['config-generator__label']}>
          {config.urlLabel}
        </label>
        <input
          id={urlInputId}
          type="text"
          value={url}
          onChange={handleUrlChange}
          placeholder={config.urlPlaceholder}
          className={styles['config-generator__input']}
        />
      </div>

      {config.extraFields?.map((field) => (
        <div key={field.key} className={styles['config-generator__field']}>
          <label htmlFor={`${provider}-${field.key}`} className={styles['config-generator__label']}>
            {field.label}
          </label>
          <input
            id={`${provider}-${field.key}`}
            type="text"
            value={extraValues[field.key] || ''}
            onChange={(e) => handleExtraChange(field.key, e.target.value)}
            placeholder={field.placeholder}
            className={styles['config-generator__input']}
          />
          {field.hint && (
            <span className={styles['config-generator__hint']}>{field.hint}</span>
          )}
        </div>
      ))}

      <div className={styles['config-generator__field']}>
        <label htmlFor={tokenInputId} className={styles['config-generator__label']}>
          {config.tokenLabel}
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
          aria-controls={helpId}
        >
          {showTokenHelp ? 'Hide instructions' : 'How to get a token?'}
        </button>

        {showTokenHelp && (
          <div id={helpId} className={styles['config-generator__help']}>
            <p className={styles['config-generator__help-title']}>
              <strong>{config.tokenHelp.title}</strong>
            </p>
            <ol className={styles['config-generator__help-list']}>
              {config.tokenHelp.steps.map((step, i) => (
                <li key={i}>
                  {step.bold ? (
                    <>
                      {step.text && `${step.text} `}Click <strong>{step.bold}</strong>
                      {i === config.tokenHelp.steps.length - 1 && ' and copy it immediately'}
                    </>
                  ) : step.scopes ? (
                    <>
                      {step.text}
                      <ul className={styles['config-generator__help-sublist']}>
                        {step.scopes.map((scope) => (
                          <li key={scope.code}>
                            <code>{scope.code}</code> — {scope.description}
                          </li>
                        ))}
                      </ul>
                    </>
                  ) : (
                    step.text
                  )}
                </li>
              ))}
            </ol>
          </div>
        )}
      </div>

      {hasInfo && (
        <section className={styles['config-generator__result']}>
          <div className={styles['config-generator__info']}>
            {infoFields.map((field) => (
              <span key={field.label}>
                <strong>{field.label}:</strong> {field.value}
              </span>
            ))}
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
