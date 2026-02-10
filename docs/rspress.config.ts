import { defineConfig } from 'rspress/config';

export default defineConfig({
  root: '.',
  base: '/devboy-tools/',
  title: 'DevBoy Tools',
  description: 'MCP server for AI coding agents with GitLab, GitHub, ClickUp, and Jira integration',
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
          ],
        },
      ],
    },
    socialLinks: [
      { icon: 'github', mode: 'link', content: 'https://github.com/meteora-pro/devboy-tools' },
    ],
  },
});
