<script setup lang="ts">
defineProps<{
  title: string;
  message: string;
  hint?: string;
}>();
</script>

<template>
  <main class="notice">
    <div class="notice-card">
      <span class="mark" aria-hidden="true">苔</span>
      <h1>{{ title }}</h1>
      <p>{{ message }}</p>
      <p v-if="hint" class="hint">{{ hint }}</p>
    </div>
  </main>
</template>

<style scoped>
.notice {
  display: grid;
  place-items: center;
  min-height: 100%;
  padding: 24px;
  background: var(--paper);
}

.notice-card {
  width: min(100%, 440px);
  padding: 32px 28px;
  text-align: center;
  background: var(--paper-raised);
  border: 1px solid var(--line);
  border-radius: 18px;
  box-shadow: 0 12px 40px rgba(38, 47, 43, 0.06);
}

.mark {
  display: inline-grid;
  place-items: center;
  width: 46px;
  height: 46px;
  margin-bottom: 14px;
  color: var(--paper);
  font-size: 20px;
  font-weight: 600;
  background: var(--moss);
  border-radius: 15px;
}

h1 {
  margin: 0 0 10px;
  font-size: 19px;
  font-weight: 600;
  letter-spacing: 0.3px;
}

p {
  margin: 0;
  color: var(--ink);
  font-size: 14px;
  line-height: 1.8;
}

.hint {
  margin-top: 8px;
  color: var(--muted);
  font-size: 12px;
}
</style>
