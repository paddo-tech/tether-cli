/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{astro,html,js,jsx,md,mdx,svelte,ts,tsx,vue}'],
  theme: {
    extend: {
      colors: {
        primary: '#00d9ff',
        accent: '#bd93f9',
        background: '#0a0e14',
        surface: '#1a1f29',
        text: '#e6e6e6',
        muted: '#8f9ba8',
        success: '#7fd962',
        warning: '#ffa500',
        error: '#ff5555',
      },
      fontFamily: {
        mono: ['"JetBrains Mono"', '"Fira Code"', '"Courier New"', 'monospace'],
        sans: ['Inter', '-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'system-ui', 'sans-serif'],
      },
    },
  },
  plugins: [],
}
