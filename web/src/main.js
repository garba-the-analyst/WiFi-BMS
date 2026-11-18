import { createApp } from 'vue'
import App from './App.vue'
import { router } from './router'
import { loadConfig, config } from './api'
import { captureHotspotParams } from './hotspot'
import { installReporter } from './errors'
import { applyBrandColour } from './theme'
import '@fontsource/inter/400.css'
import '@fontsource/inter/500.css'
import '@fontsource/inter/600.css'
import '@fontsource/space-grotesk/500.css'
import '@fontsource/space-grotesk/600.css'
import '@fontsource/space-grotesk/700.css'
import './style.css'

captureHotspotParams()
loadConfig()
  .catch(() => {})
  .finally(() => {
    applyBrandColour(config().primary)
    const app = createApp(App).use(router)
    installReporter(app)
    app.mount('#app')
  })
