/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{svelte,ts}'],
  theme: {
    extend: {
      colors: {
        bg:       '#0f1115',
        panel:    '#161a22',
        panel2:   '#1c212c',
        panel3:   '#232936',
        border:   '#2a3142',
        text:     '#dde3ef',
        muted:    '#7c879b',
        accent:   '#5a9eff',
        accent2:  '#7c5aff',
        ok:       '#4caf7d',
        warn:     '#e6a23c',
        err:      '#e05c5c',
      },
      fontFamily: {
        mono: ['ui-monospace', 'SFMono-Regular', 'Menlo', 'Consolas', 'monospace'],
      },
      borderRadius: { DEFAULT: '8px', sm: '6px' },
    },
  },
  plugins: [],
};
