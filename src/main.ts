import { createApp } from 'vue'
import { createPinia } from 'pinia'
import naive from 'naive-ui'
import App from './App.vue'
import router from './router'
import './assets/variables.css'
import './assets/main.css'

// Disable auto-capitalization/correction on every editable element.
// Tauri/macOS WKWebView does not reliably propagate body-level
// autocapitalize=off to the real inputs rendered by Naive UI, so we set
// the attributes directly on each node and watch for dynamically added ones
// (modals, lazily mounted forms, etc.).
function disableAutocapitalize(root: ParentNode) {
  const nodes = root.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLElement>(
    'input, textarea, [contenteditable]'
  )
  nodes.forEach((el) => {
    el.setAttribute('autocapitalize', 'off')
    el.setAttribute('autocorrect', 'off')
    el.setAttribute('spellcheck', 'false')
  })
}

disableAutocapitalize(document)

const observer = new MutationObserver((mutations) => {
  for (const m of mutations) {
    m.addedNodes.forEach((node) => {
      if (node.nodeType !== Node.ELEMENT_NODE) return
      const el = node as Element
      // The added node itself may be editable, or may contain editable children.
      if (
        el.matches?.('input, textarea, [contenteditable]') ||
        el.querySelector?.('input, textarea, [contenteditable]')
      ) {
        disableAutocapitalize(el)
      }
    })
  }
})
observer.observe(document.body, { childList: true, subtree: true })

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.use(naive)
app.mount('#app')
