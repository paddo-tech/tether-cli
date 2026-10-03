# Tether CLI website

The marketing site for [Tether CLI](https://github.com/paddo-tech/tether-cli).

The site uses Astro and the Relay design. It supports desktop, mobile, light mode, and dark mode.

## Development

```bash
bun install --frozen-lockfile
bun run dev
```

Open `http://localhost:4321`.

## Production build

```bash
bun run build
bun run preview
```

Astro writes the static site to `dist/`.

## Pages

| Route | Content |
|-------|---------|
| `/` | Product overview, supply-chain checks, machine profiles, setup, and installation |
| `/docs` | Upgrade guide, Linux, commands, configuration, package security, machine trust, and troubleshooting |
| `/teams` | Team repositories, recipient keys, and project sharing |
| `/security` | Encryption, key management, package checks, machine trust, scanning, and security boundaries |
| `/designs` | Original design comparison, excluded from search indexing |

## Design

`src/styles/global.css` defines the Relay colors, typography, spacing, and responsive layouts.

The site uses Space Grotesk, IBM Plex Sans, and IBM Plex Mono.

`BaseLayout.astro` supplies shared navigation, footer, metadata, and theme initialization.

The theme control saves each selection in local storage. The first visit uses the system theme.

Diagrams use SVG icons and HTML labels. The flow diagrams change direction on narrow screens.

| Component | Purpose |
|-----------|---------|
| `NetworkDiagram.astro` | Show machines connected through Git |
| `ProfileDiagram.astro` | Compare machine profiles |
| `TeamDiagram.astro` | Show encrypted sharing between recipients |
| `FlowDiagram.astro` | Explain sync and encryption steps |
| `Code.astro` | Display and copy commands |
| `InstallSection.astro` | Show the Homebrew installation commands |

## Brand assets

- `src/assets/favicon.svg` contains the link icon. Astro adds a content hash to its URL for cache updates.
- `public/og-image.png` is the 1200 × 630 social sharing image.
- `public/og-image.svg` is its vector source. Text uses paths to preserve its appearance.

`BaseLayout.astro` defines Open Graph and Twitter image metadata with absolute URLs.

## Deployment

The repository uses Fly.io for website hosting.

The deployment workflow is `.github/workflows/deploy-website.yml` at the repository root.

```bash
fly deploy
```

Run the deployment command from this directory.
