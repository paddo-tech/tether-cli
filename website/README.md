# Tether CLI Website

Marketing website for Tether CLI - an open source tool that automatically syncs your development environment across Macs with end-to-end encryption.

This website is part of the [Tether CLI monorepo](https://github.com/paddo/tether-cli).

Built with Astro.js, Tailwind v4, and deployed on Fly.io.

## Tech Stack

- **Framework**: [Astro.js](https://astro.build) v5
- **Styling**: [Tailwind CSS v4](https://tailwindcss.com) (CSS-first approach)
- **Package Manager**: [Bun](https://bun.sh)
- **Deployment**: [Fly.io](https://fly.io)
- **Fonts**: JetBrains Mono (monospace), Inter (sans-serif)

## Features

- 🎨 **Developer-first design** with terminal aesthetics
- 🌙 **Dark theme** by default
- 📱 **Fully responsive** mobile-first design
- ⚡️ **Fast** - Astro static site generation
- 🔍 **SEO optimized** with meta tags and Open Graph
- 📊 **GitHub integration** - Live star count on homepage
- 🎭 **Animated terminal demo** - Interactive command showcase
- 🔐 **Comprehensive security page** - Full transparency on encryption
- 📚 **Documentation hub** - Sidebar navigation and markdown support

## Project Structure

```
tether-cli-website/
├── src/
│   ├── components/
│   │   ├── Navbar.astro
│   │   ├── Footer.astro
│   │   ├── AnimatedTerminal.astro
│   │   ├── CodeBlock.astro
│   │   ├── FeatureCard.astro
│   │   ├── GitHubStars.astro
│   │   └── InstallCommand.astro
│   ├── layouts/
│   │   ├── BaseLayout.astro
│   │   └── DocsLayout.astro
│   ├── pages/
│   │   ├── index.astro (home)
│   │   ├── docs/
│   │   │   └── index.astro
│   │   └── security.astro
│   └── styles/
│       └── global.css (Tailwind v4 config)
├── public/
│   ├── logo.svg
│   └── favicon.svg
├── Dockerfile
├── nginx.conf
├── fly.toml
└── astro.config.mjs
```

## Development

### Prerequisites

- [Bun](https://bun.sh) installed
- Git configured

### Setup

```bash
# Clone the repository
git clone https://github.com/paddo-tech/tether-cli-website.git
cd tether-cli-website

# Install dependencies
bun install

# Start development server
bun run dev
```

The site will be available at `http://localhost:4321`

### Available Commands

```bash
# Development
bun run dev          # Start dev server with hot reload
bun run build        # Build for production
bun run preview      # Preview production build locally

# Type checking
bun run check        # Check for TypeScript errors
```

## Deployment

### Deploy to Fly.io

1. **Install Fly CLI**:
   ```bash
   curl -L https://fly.io/install.sh | sh
   ```

2. **Login to Fly.io**:
   ```bash
   fly auth login
   ```

3. **Create the app** (first time only):
   ```bash
   fly apps create tether-cli-website --org paddo-tech
   ```

4. **Deploy**:
   ```bash
   fly deploy
   ```

5. **Set up custom domain** (optional):
   ```bash
   fly certs add tether-cli.com
   fly certs add www.tether-cli.com
   ```

   Then configure DNS:
   - A record: `tether-cli.com` → Fly.io IP
   - CNAME record: `www` → `tether-cli-website.fly.dev`

### Environment Variables

The site doesn't require any environment variables in production. GitHub API calls for star counts happen at build time.

### Continuous Deployment

To set up automatic deployments on push:

1. Generate a Fly.io deploy token:
   ```bash
   fly tokens create deploy -x 999999h
   ```

2. Add as a GitHub secret named `FLY_API_TOKEN`

3. Create `.github/workflows/deploy.yml`:
   ```yaml
   name: Deploy to Fly.io
   on:
     push:
       branches: [main]
   jobs:
     deploy:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: superfly/flyctl-actions/setup-flyctl@master
         - run: flyctl deploy --remote-only
           env:
             FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN }}
   ```

## Customization

### Update GitHub Repository Links

Replace all instances of `username/tether-cli` with the actual GitHub repository:

```bash
# In components and pages
grep -r "username/tether-cli" src/
```

Update in:
- `src/components/GitHubStars.astro`
- `src/components/Navbar.astro`
- `src/components/Footer.astro`
- `src/pages/index.astro`
- `src/pages/security.astro`

### Customize Branding

Colors are defined in `src/styles/global.css` using Tailwind v4's `@theme` directive:

```css
@theme {
  --color-primary: #00d9ff;    /* Cyan */
  --color-accent: #bd93f9;     /* Purple */
  --color-background: #0a0e14; /* Dark */
  /* ... */
}
```

### Add More Documentation Pages

Create new `.astro` or `.md` files in `src/pages/docs/`:

```astro
---
import DocsLayout from '../../layouts/DocsLayout.astro';
---

<DocsLayout title="Your Page Title">
  <h1>Page Content</h1>
  <!-- ... -->
</DocsLayout>
```

Update the sidebar navigation in `src/layouts/DocsLayout.astro`.

## Performance

The site is optimized for performance:

- **Static site generation** - Pre-rendered HTML
- **Minimal JavaScript** - Only for interactive components
- **Image optimization** - SVG logo/favicon
- **Gzip compression** - Enabled in nginx
- **Asset caching** - 1 year cache for static assets
- **CDN** - Fly.io edge caching

Target Lighthouse scores:
- Performance: >95
- Accessibility: >95
- Best Practices: >95
- SEO: >95

## Browser Support

- Chrome/Edge (last 2 versions)
- Firefox (last 2 versions)
- Safari (last 2 versions)
- Mobile browsers (iOS Safari, Chrome Mobile)

## Contributing

Contributions to improve the website are welcome!

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Test locally with `bun run build && bun run preview`
5. Submit a pull request

## License

This website is part of the Tether CLI project and is licensed under the MIT License.

## Support

For issues with the website:
- Open an issue in this repository
- For Tether CLI issues, use the [main repo](https://github.com/username/tether-cli)

## Links

- **Main Project**: [Tether CLI](https://github.com/username/tether-cli)
- **Live Site**: [tether-cli.com](https://tether-cli.com)
- **Documentation**: [tether-cli.com/docs](https://tether-cli.com/docs)
- **Security**: [tether-cli.com/security](https://tether-cli.com/security)
