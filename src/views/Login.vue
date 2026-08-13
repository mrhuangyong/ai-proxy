<template>
  <div class="login-shell">
    <main class="login-card fade-in" style="animation-delay: 0.1s">
      <div class="card-accent" />

      <header class="card-header">
        <div class="brand">
          <svg class="brand-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 2L2 7l10 5 10-5-10-5z"/>
            <path d="M2 17l10 5 10-5"/>
            <path d="M2 12l10 5 10-5"/>
          </svg>
          <span class="brand-name">AI Proxy</span>
        </div>
        <p class="brand-tagline">本地 LLM 统一网关</p>
      </header>

      <h1 class="card-title fade-in" style="animation-delay: 0.2s">
        管理员登录
      </h1>
      <p class="card-lead fade-in" style="animation-delay: 0.25s">
        输入凭据以访问控制台
      </p>

      <form class="login-form fade-in" style="animation-delay: 0.35s" @submit.prevent="handleSubmit">
        <div class="field">
          <label class="field-label" for="username">用户名</label>
          <n-input
            id="username"
            v-model:value="form.username"
            placeholder="请输入用户名"
            :input-props="{ autocomplete: 'username', spellcheck: 'false' }"
            class="field-input"
            @keyup.enter="handleSubmit"
          />
        </div>

        <div class="field">
          <label class="field-label" for="password">密码</label>
          <n-input
            id="password"
            v-model:value="form.password"
            type="password"
            show-password-on="click"
            placeholder="请输入密码"
            :input-props="{ autocomplete: 'current-password' }"
            class="field-input"
            @keyup.enter="handleSubmit"
          />
        </div>

        <button
          type="submit"
          class="submit-btn"
          :class="{ loading: submitting }"
          :disabled="submitting"
        >
          <span v-if="!submitting" class="submit-label">
            进入控制台
          </span>
          <span v-else class="submit-loading">登录中…</span>
        </button>
      </form>

      <footer class="card-footer fade-in" style="animation-delay: 0.5s">
        <span class="footer-item">v{{ version }}</span>
        <span class="footer-dot" />
        <span class="footer-item">SYSTEM ONLINE</span>
      </footer>
    </main>

    <div class="bg-shape shape-1" />
    <div class="bg-shape shape-2" />
  </div>
</template>

<script setup lang="ts">
import { reactive, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useMessage } from 'naive-ui'
import { useAuthStore } from '../stores/auth'
import pkg from '../../package.json'

const router = useRouter()
const route = useRoute()
const message = useMessage()
const authStore = useAuthStore()

const version = pkg.version

const submitting = ref(false)

const form = reactive({
  username: '',
  password: '',
})

async function handleSubmit() {
  if (!form.username || !form.password) {
    message.error('请输入用户名和密码')
    return
  }
  submitting.value = true
  try {
    await authStore.login(form.username, form.password)
    const redirect = (route.query.redirect as string) || '/'
    router.replace(redirect)
  } catch (err) {
    const msg = err instanceof Error ? err.message : '登录失败'
    message.error(msg)
  } finally {
    submitting.value = false
  }
}
</script>

<style scoped>
.login-shell {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  background: #FDFBF7;
  color: #1A1A1A;
  font-family: var(--font-sans);
  overflow: hidden;
  padding: 48px 24px;
}

:deep(.n-layout-scroll-container) {
  overflow: hidden !important;
}

/* ===== BACKGROUND SHAPES ===== */
.bg-shape {
  position: absolute;
  border-radius: 50%;
  filter: blur(80px);
  pointer-events: none;
  opacity: 0.55;
}

.shape-1 {
  width: 520px;
  height: 520px;
  top: -180px;
  right: -120px;
  background: radial-gradient(circle, rgba(8, 145, 178, 0.14), transparent 70%);
}

.shape-2 {
  width: 420px;
  height: 420px;
  bottom: -160px;
  left: -100px;
  background: radial-gradient(circle, rgba(20, 23, 32, 0.08), transparent 70%);
}

/* ===== LOGIN CARD ===== */
.login-card {
  position: relative;
  z-index: 1;
  width: 100%;
  max-width: 420px;
  background: rgba(255, 255, 255, 0.92);
  border: 1px solid rgba(26, 26, 26, 0.08);
  padding: 56px 48px 44px;
  box-shadow:
    0 24px 70px rgba(26, 26, 26, 0.08),
    0 2px 8px rgba(26, 26, 26, 0.04);
  backdrop-filter: blur(12px);
}

.card-accent {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  height: 3px;
  background: var(--accent);
}

.card-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 40px;
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
}

.brand-icon {
  color: var(--accent);
}

.brand-name {
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.02em;
  color: #1A1A1A;
}

.brand-tagline {
  font-family: var(--font-mono);
  font-size: 10px;
  color: #9BA3B1;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  margin: 0;
}

.card-title {
  font-size: 38px;
  font-weight: 600;
  letter-spacing: -0.03em;
  line-height: 1.1;
  margin: 0 0 10px;
  color: #1A1A1A;
}

.card-lead {
  font-size: 14px;
  color: #5C6370;
  margin: 0 0 36px;
}

/* ===== FORM ===== */
.login-form {
  display: flex;
  flex-direction: column;
  gap: 24px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.field-label {
  font-family: var(--font-mono);
  font-size: 10px;
  color: #5C6370;
  letter-spacing: 0.15em;
  text-transform: uppercase;
}

.field-input {
  background: #FFFFFF !important;
  color-scheme: light;
}

.field-input :deep(.n-input__border),
.field-input :deep(.n-input__state-border) {
  border: 1px solid rgba(26, 26, 26, 0.1) !important;
  border-radius: 4px !important;
  box-shadow: none !important;
  transition: border-color 0.2s, box-shadow 0.2s !important;
}

.field-input :deep(.n-input:hover .n-input__state-border),
.field-input :deep(.n-input:focus-within .n-input__state-border),
.field-input :deep(.n-input--focus .n-input__state-border) {
  border-color: var(--accent) !important;
  box-shadow: 0 0 0 3px rgba(8, 145, 178, 0.08) !important;
}

.field-input :deep(.n-input__input-el),
.field-input :deep(.n-input__textarea-el) {
  color: #1A1A1A !important;
  font-family: var(--font-mono) !important;
  font-size: 14px !important;
  caret-color: var(--accent);
}

.field-input :deep(.n-input__placeholder) {
  color: #9BA3B1 !important;
  font-family: var(--font-mono) !important;
}

.field-input :deep(.n-input__suffix) {
  color: #9BA3B1 !important;
}

.field-input :deep(.n-input__suffix .n-base-icon:hover) {
  color: var(--accent) !important;
}

/* ===== SUBMIT BUTTON ===== */
.submit-btn {
  margin-top: 8px;
  height: 48px;
  background: var(--accent);
  border: none;
  color: #FFFFFF;
  font-family: var(--font-sans);
  font-size: 14px;
  font-weight: 600;
  letter-spacing: 0.02em;
  cursor: pointer;
  border-radius: 4px;
  transition: transform 0.15s, background 0.2s, box-shadow 0.2s;
}

.submit-btn:not(:disabled):hover {
  background: var(--accent-hover);
  transform: translateY(-1px);
  box-shadow: 0 8px 20px rgba(8, 145, 178, 0.25);
}

.submit-btn:not(:disabled):active {
  transform: translateY(0);
}

.submit-btn:disabled {
  cursor: not-allowed;
  opacity: 0.75;
}

.submit-label,
.submit-loading {
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.submit-loading {
  animation: flicker 1.2s ease-in-out infinite;
}

@keyframes flicker {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

/* ===== FOOTER ===== */
.card-footer {
  margin-top: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  font-family: var(--font-mono);
  font-size: 10px;
  color: #9BA3B1;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.footer-dot {
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: #16A34A;
}

/* ===== ENTRANCE ANIMATION ===== */
.fade-in {
  opacity: 0;
  animation: fadeInUp 0.7s cubic-bezier(0.16, 1, 0.3, 1) forwards;
}

@keyframes fadeInUp {
  from {
    opacity: 0;
    transform: translateY(14px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* ===== RESPONSIVE ===== */
@media (max-width: 520px) {
  .login-shell {
    padding: 0;
    align-items: stretch;
  }

  .login-card {
    max-width: none;
    min-height: 100vh;
    border: none;
    box-shadow: none;
    padding: 48px 28px 36px;
    background: rgba(255, 255, 255, 0.96);
  }

  .card-title {
    font-size: 32px;
  }

  .card-header {
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    margin-bottom: 32px;
  }
}
</style>
