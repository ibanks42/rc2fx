import { readFileSync } from 'node:fs'
import { posix } from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitepress'

const repo = 'https://github.com/ibanks42/rc2fx'
const docs = fileURLToPath(new URL('..', import.meta.url))

// A crate page is a single include of that crate's README, so its links are
// relative to the README rather than to the page.
function linkBase(page) {
  const include = readFileSync(docs + page, 'utf8').match(/^<!--@include: (.+?)-->/m)
  return include ? posix.join('docs', posix.dirname(page), include[1]) : `docs/${page}`
}

// The pages link to files by their repository paths, so the links also work
// on GitHub. On the site, a page under docs/ stays a page and anything else
// opens on GitHub.
function siteLink(href, page) {
  if (!href || href.startsWith('#') || href.startsWith('/') || /^[a-z][a-z0-9+.-]*:/i.test(href)) {
    return href
  }
  const [path, anchor = ''] = href.split(/(?=#)/)
  const target = posix.normalize(posix.join(posix.dirname(linkBase(page)), path))
  if (target.startsWith('docs/') && target.endsWith('.md')) {
    return posix.relative(posix.dirname(`docs/${page}`), target) + anchor
  }
  return `${repo}/blob/main/${target}${anchor}`
}

// Heading anchors as GitHub makes them, so links like `building.md#3-package-it`
// work here, on GitHub and in the release package alike. Unlike GitHub, dots
// are kept so the generated update-history headings (`## v1.0.0`) get stable
// `#v1.0.0` anchors.
function slugify(text) {
  return text.trim().toLowerCase().replace(/[^\p{L}\p{M}\p{N}\s_.-]/gu, '').replace(/\s/g, '-')
}

export default defineConfig({
  title: 'RC2FX',
  description: 'Extra audio processing for the RØDECaster Pro II',
  base: '/rc2fx/',
  cleanUrls: true,
  appearance: 'dark',
  outDir: '../target/docs',
  cacheDir: '../target/docs-cache',
  rewrites: { 'introduction.md': 'index.md' },
  // Source notes for the generated update histories, not pages themselves.
  srcExclude: ['updates/source/**'],
  markdown: {
    anchor: { slugify },
    config(md) {
      const open = md.renderer.rules.link_open
      md.renderer.rules.link_open = (tokens, index, options, env, self) => {
        const token = tokens[index]
        // The source file's path: `relativePath` is the page after `rewrites`.
        const page = posix.relative(docs, env.realPath ?? docs + env.relativePath)
        token.attrSet('href', siteLink(token.attrGet('href'), page))
        return open ? open(tokens, index, options, env, self) : self.renderToken(tokens, index, options)
      }
    },
  },
  themeConfig: {
    search: { provider: 'local' },
    outline: [2, 3],
    nav: [
      { text: 'Guide', link: '/user/getting-started' },
      { text: 'Releases', link: '/updates/' },
    ],
    socialLinks: [{ icon: 'github', link: repo }],
    sidebar: [
      { text: 'Introduction', link: '/' },
      {
        text: 'Using RC2FX',
        items: [
          { text: 'Getting started', link: '/user/getting-started' },
          { text: 'Installing RC2FX', link: '/user/installing' },
          { text: 'Using the app', link: '/user/using-the-app' },
          { text: 'Effects', link: '/user/effects' },
          { text: 'Noise suppression', link: '/user/noise-suppression' },
          { text: 'Settings and diagnostics', link: '/user/settings-and-diagnostics' },
          { text: 'How it works', link: '/user/how-it-works' },
          { text: 'Tips and troubleshooting', link: '/user/troubleshooting' },
        ],
      },
      {
        text: 'Updates',
        items: [
          { text: 'Release history', link: '/updates/' },
          { text: 'App history', link: '/updates/app' },
          { text: 'Mixer history', link: '/updates/mixer' },
        ],
      },
      {
        text: 'Developing RC2FX',
        collapsed: true,
        items: [
          { text: 'Overview', link: '/developer/' },
          {
            text: 'Building from source',
            link: '/developer/building',
            items: [{ text: 'Linux setup', link: '/developer/linux' }],
          },
          { text: 'Working on a mixer', link: '/developer/mixer-workflow' },
          { text: 'Commands', link: '/developer/commands' },
          { text: 'Architecture', link: '/developer/architecture' },
          { text: 'Releases', link: '/developer/releases' },
          { text: 'Update history', link: '/developer/update-history' },
        ],
      },
      {
        text: 'Technical reference',
        collapsed: true,
        items: [
          { text: 'Overview', link: '/reference/' },
          { text: 'Noise suppression internals', link: '/reference/suppression' },
          { text: 'Installer internals', link: '/reference/installer' },
          { text: 'USB control', link: '/reference/usb-control' },
          { text: 'Controller API and settings', link: '/reference/api' },
          { text: 'EQ format and EasyEffects', link: '/reference/eq-format' },
          { text: 'Firmware support', link: '/reference/firmware' },
          { text: 'Stock source names', link: '/reference/source-name-evidence' },
          { text: 'Virtual USB paths', link: '/reference/usb-paths' },
          {
            text: 'RØDECaster Duo',
            link: '/reference/duo',
            items: [{ text: 'Duo audio routing', link: '/reference/duo-routing' }],
          },
          { text: 'Hardware', link: '/reference/hardware' },
          {
            text: 'Crates',
            items: [
              { text: 'rc2fx-app', link: '/crates/rc2fx-app' },
              { text: 'rc2fx-core', link: '/crates/rc2fx-core' },
              { text: 'rc2fx-dsp', link: '/crates/rc2fx-dsp' },
              { text: 'rc2fx-suppression', link: '/crates/rc2fx-suppression' },
              { text: 'rc2fx-dfn weights', link: '/crates/rc2fx-dfn' },
              { text: 'rc2fx-native', link: '/crates/rc2fx-native' },
              { text: 'rc2fx-gui', link: '/crates/rc2fx-gui' },
              { text: 'rc2fx-native-control', link: '/crates/rc2fx-native-control' },
              { text: 'rc2fx-transport', link: '/crates/rc2fx-transport' },
            ],
          },
        ],
      },
    ],
  },
})
