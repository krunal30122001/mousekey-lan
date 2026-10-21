<script setup lang="ts">
defineProps<{ label: string; description?: string; disabled?: boolean }>();
const model = defineModel<boolean>({ required: true });
</script>

<template>
  <label class="flex cursor-pointer items-start justify-between gap-6 py-3" :class="{ 'opacity-50': disabled }">
    <span>
      <span class="block text-sm font-medium">{{ label }}</span>
      <span v-if="description" class="mt-0.5 block text-xs muted">{{ description }}</span>
    </span>
    <span class="relative mt-0.5 inline-flex shrink-0">
      <input v-model="model" type="checkbox" class="peer sr-only" :disabled="disabled" />
      <span
        class="h-5 w-9 rounded-full bg-slate-300 transition-colors peer-checked:bg-indigo-600 peer-focus-visible:ring-2 peer-focus-visible:ring-indigo-500/50 dark:bg-slate-700"
      />
      <span
        class="absolute top-0.5 left-0.5 h-4 w-4 rounded-full bg-white shadow transition-transform peer-checked:translate-x-4"
      />
    </span>
  </label>
</template>
