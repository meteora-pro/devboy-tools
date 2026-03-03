import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@rspress/core';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root: path.join(__dirname, 'guide'),
  globalStyles: path.join(__dirname, 'styles/index.css'),
  base: process.env.DOCS_BASE_PATH || '/',
  title: 'DevBoy tools',
  description:
    'MCP server for AI coding agents with GitHub, GitLab, ClickUp, and Jira integration',
  themeConfig: {
    socialLinks: [
      {
        icon: 'github',
        mode: 'link',
        content: 'https://github.com/meteora-pro/devboy-tools',
      },
    ],
  },
});
