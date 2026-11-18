// One brand colour per establishment. Hover and dark-mode shades are derived from it.
// The site's content-security policy forbids inline <style> elements, so this sets CSS variables
// on the root element directly and follows the light/dark preference.
import { isHexColour, mixHex } from './format'

export function applyBrandColour(primary) {
  if (!isHexColour(primary)) return
  const mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null
  const set = () => {
    const root = document.documentElement
    const dark = mq && mq.matches && root.dataset.theme !== 'light'
    root.style.setProperty('--brand', dark ? mixHex(primary, '#ffffff', 0.4) : primary)
    root.style.setProperty('--brand-ink', dark ? mixHex(primary, '#ffffff', 0.6) : mixHex(primary, '#000000', 0.25))
  }
  set()
  if (mq && mq.addEventListener) mq.addEventListener('change', set)
}
