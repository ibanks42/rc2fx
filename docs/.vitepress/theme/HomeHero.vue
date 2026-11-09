<script setup>
import { onMounted, ref } from 'vue'
import { withBase } from 'vitepress'

const repo = 'https://github.com/ibanks42/rc2fx'
const file = (name) => `${repo}/releases/latest/download/${name}`

const platforms = [
  {
    id: 'windows',
    name: 'Windows',
    arch: 'x64',
    links: [
      { label: 'Installer (.exe)', short: 'Download for Windows', href: file('rc2fx-windows-x64-setup.exe') },
      { label: 'Portable (.zip)', short: 'Portable .zip', href: file('rc2fx-windows-x64.zip') },
    ],
  },
  {
    id: 'macos',
    name: 'macOS',
    arch: 'Apple silicon',
    links: [
      { label: 'Installer (.pkg)', short: 'Download for macOS', href: file('rc2fx-macos-arm64.pkg') },
      { label: 'Portable (.zip)', short: 'Portable .zip', href: file('rc2fx-macos-arm64.zip') },
    ],
  },
  {
    id: 'linux',
    name: 'Linux',
    arch: 'x64',
    links: [
      { label: 'AppImage', short: 'Download for Linux', href: file('rc2fx-linux-x64.AppImage') },
    ],
  },
]

// Known only in the browser; the row keeps its height until then.
const mine = ref(null)
const version = ref(null)

// The user agent first: it is what every browser sends and what overrides
// change. navigator.platform is only a fallback for an unrecognised agent.
function detect() {
  const ua = navigator.userAgent.toLowerCase()
  if (/android|iphone|ipad|ipod|mobile/.test(ua)) return null
  if (ua.includes('windows')) return 'windows'
  if (ua.includes('macintosh') || ua.includes('mac os x')) return 'macos'
  if (ua.includes('linux') || ua.includes('x11')) return 'linux'
  const p = (navigator.userAgentData?.platform || navigator.platform || '').toLowerCase()
  if (p.includes('win')) return 'windows'
  if (p.includes('mac')) return 'macos'
  if (p.includes('linux')) return 'linux'
  return null
}

onMounted(async () => {
  mine.value = platforms.find((p) => p.id === detect()) ?? null
  try {
    const response = await fetch('https://api.github.com/repos/ibanks42/rc2fx/releases/latest')
    if (response.ok) version.value = (await response.json()).tag_name ?? null
  } catch {
    // The version is a nicety; the links work without it.
  }
})
</script>

<template>
  <section class="hero">
    <h1 class="name">RC2FX</h1>
    <p class="tagline">Extra audio processing for the RØDECaster Pro II</p>

    <div class="primary">
      <template v-if="mine">
        <a
          v-for="(link, i) in mine.links"
          :key="link.href"
          :class="['btn', i === 0 ? 'brand' : 'alt']"
          :href="link.href"
        >{{ link.short }}</a>
      </template>
      <a v-else class="btn brand" href="#downloads">Download</a>
      <a class="btn alt" :href="withBase('/user/getting-started')">Getting started</a>
    </div>
    <p class="meta">
      <span v-if="version">{{ version }} · </span>
      <a :href="`${repo}/releases/latest`">Release notes and all files</a>
    </p>

    <div id="downloads" class="grid">
      <div v-for="p in platforms" :key="p.id" :class="['card', { mine: mine?.id === p.id }]">
        <div class="pname">{{ p.name }}</div>
        <div class="arch">{{ p.arch }}<span v-if="mine?.id === p.id"> · your system</span></div>
        <a v-for="link in p.links" :key="link.href" :href="link.href">{{ link.label }}</a>
      </div>
    </div>
  </section>
</template>

<style scoped>
.hero {
  max-width: 760px;
  margin: 0 auto;
  padding: 64px 24px 40px;
  text-align: center;
}
.name {
  display: inline-block;
  font-size: 56px;
  line-height: 64px;
  font-weight: 700;
  letter-spacing: -0.5px;
  background: -webkit-linear-gradient(120deg, #bd34fe 30%, #41d1ff);
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
}
.tagline {
  margin: 12px 0 28px;
  font-size: 20px;
  line-height: 28px;
  color: var(--vp-c-text-2);
}
.primary {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 12px;
  min-height: 40px;
}
.btn {
  display: inline-block;
  border: 1px solid transparent;
  border-radius: 20px;
  padding: 0 20px;
  line-height: 38px;
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  transition: background-color 0.25s, border-color 0.25s;
}
.btn.brand {
  background: var(--vp-button-brand-bg);
  border-color: var(--vp-button-brand-border);
  color: var(--vp-button-brand-text);
}
.btn.brand:hover {
  background: var(--vp-button-brand-hover-bg);
}
.btn.alt {
  background: var(--vp-button-alt-bg);
  border-color: var(--vp-button-alt-border);
  color: var(--vp-button-alt-text);
}
.btn.alt:hover {
  background: var(--vp-button-alt-hover-bg);
}
.meta {
  margin: 12px 0 0;
  min-height: 20px;
  font-size: 13px;
  color: var(--vp-c-text-2);
}
.meta a,
.card a {
  color: var(--vp-c-brand-1);
}
.meta a:hover,
.card a:hover {
  text-decoration: underline;
}
.grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
  margin-top: 32px;
  text-align: left;
}
.card {
  border: 1px solid var(--vp-c-divider);
  border-radius: 12px;
  padding: 16px 18px;
  background: var(--vp-c-bg-soft);
}
.card.mine {
  border-color: var(--vp-c-brand-1);
}
.pname {
  font-weight: 600;
}
.arch {
  margin-bottom: 8px;
  font-size: 12px;
  color: var(--vp-c-text-2);
}
.card a {
  display: block;
  font-size: 14px;
  line-height: 28px;
}
@media (max-width: 640px) {
  .name {
    font-size: 44px;
    line-height: 52px;
  }
  .grid {
    grid-template-columns: 1fr;
  }
}
</style>
