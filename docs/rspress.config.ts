import { defineConfig } from 'rspress/config';

export default defineConfig({
  root: '.',
  base: process.env.DOCS_BASE_PATH || '/',
  title: 'DevBoy Tools',
  description: 'MCP server for AI coding agents with GitHub, GitLab, ClickUp, and Jira integration',
  themeConfig: {
    nav: [
      { text: 'Getting Started', link: '/getting-started/' },
      { text: 'Integrations', link: '/integrations/github' },
      { text: 'GitHub', link: 'https://github.com/meteora-pro/devboy-tools' },
    ],
    sidebar: {
      '/getting-started/': [
        {
          text: 'Getting Started',
          items: [
            { text: 'Installation', link: '/getting-started/' },
            { text: 'Quick Start', link: '/getting-started/quick-start' },
          ],
        },
      ],
      '/integrations/': [
        {
          text: 'Integrations',
          items: [
            { text: 'GitHub', link: '/integrations/github' },
            { text: 'GitLab', link: '/integrations/gitlab' },
            { text: 'ClickUp', link: '/integrations/clickup' },
            { text: 'Jira', link: '/integrations/jira' },
          ],
        },
      ],
    },
    socialLinks: [
      { icon: 'github', mode: 'link', content: 'https://github.com/meteora-pro/devboy-tools' },
    ],
  },
});
