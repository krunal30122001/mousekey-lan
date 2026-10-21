<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { api, errorText } from "../api";
import { toast, useEngine } from "../composables/useEngine";
import type { LayoutDevice, Rect } from "../types";

interface Item extends LayoutDevice {
  bw: number;
  bh: number;
  bx: number;
  by: number;
}

const { state, act } = useEngine();
const items = ref<Item[]>([]);
const dirty = ref(false);
const canvas = ref<HTMLDivElement | null>(null);
const view = reactive({ scale: 0.08, minX: 0, minY: 0, width: 800 });
const drag = reactive({ id: "", startX: 0, startY: 0, origX: 0, origY: 0 });

const PAD = 40;
const SNAP_PX = 22;

function bbox(screens: Rect[]) {
  if (!screens.length) return { bx: 0, by: 0, bw: 1920, bh: 1080 };
  const l = Math.min(...screens.map((s) => s.x));
  const t = Math.min(...screens.map((s) => s.y));
  const r = Math.max(...screens.map((s) => s.x + s.w));
  const b = Math.max(...screens.map((s) => s.y + s.h));
  return { bx: l, by: t, bw: r - l, bh: b - t };
}

async function load() {
  try {
    const v = await api.layout();
    items.value = v.devices.map((d) => ({ ...d, ...bbox(d.screens) }));
    dirty.value = false;
    fit();
  } catch (e) {
    toast("error", errorText(e));
  }
}

/** Scale so the whole arrangement fits the canvas (recomputed only when not dragging). */
function fit() {
  const el = canvas.value;
  if (!el || !items.value.length) return;
  view.width = el.clientWidth;
  const minX = Math.min(...items.value.map((i) => i.x));
  const minY = Math.min(...items.value.map((i) => i.y));
  const maxX = Math.max(...items.value.map((i) => i.x + i.bw));
  const maxY = Math.max(...items.value.map((i) => i.y + i.bh));
  const spanX = maxX - minX + 2600;
  const spanY = maxY - minY + 1600;
  view.scale = Math.min((el.clientWidth - PAD * 2) / spanX, (420 - PAD * 2) / spanY);
  view.minX = minX - 1300;
  view.minY = minY - 800;
}

const canvasHeight = computed(() => {
  if (!items.value.length) return 420;
  const maxY = Math.max(...items.value.map((i) => i.y + i.bh));
  return Math.max(420, (maxY - view.minY) * view.scale + PAD * 2);
});

function boxStyle(i: Item) {
  return {
    left: `${(i.x - view.minX) * view.scale + PAD}px`,
    top: `${(i.y - view.minY) * view.scale + PAD}px`,
    width: `${i.bw * view.scale}px`,
    height: `${i.bh * view.scale}px`,
  };
}

function monitorStyle(i: Item, s: Rect) {
  return {
    left: `${(s.x - i.bx) * view.scale}px`,
    top: `${(s.y - i.by) * view.scale}px`,
    width: `${s.w * view.scale}px`,
    height: `${s.h * view.scale}px`,
  };
}

function onPointerDown(e: PointerEvent, i: Item) {
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  Object.assign(drag, { id: i.id, startX: e.clientX, startY: e.clientY, origX: i.x, origY: i.y });
}

function onPointerMove(e: PointerEvent) {
  const i = items.value.find((d) => d.id === drag.id);
  if (!i) return;
  i.x = Math.round(drag.origX + (e.clientX - drag.startX) / view.scale);
  i.y = Math.round(drag.origY + (e.clientY - drag.startY) / view.scale);
  dirty.value = true;
}

function onPointerUp() {
  const i = items.value.find((d) => d.id === drag.id);
  drag.id = "";
  if (!i) return;
  snap(i);
  fit();
}

/** Snaps a dropped device flush against the nearest edge of another device. */
function snap(i: Item) {
  const t = SNAP_PX / view.scale;
  let best: { dx: number; dy: number; d: number } | null = null;
  for (const o of items.value) {
    if (o.id === i.id) continue;
    const xs = [o.x + o.bw - i.x, o.x - (i.x + i.bw), o.x - i.x, o.x + o.bw - (i.x + i.bw)];
    const ys = [o.y + o.bh - i.y, o.y - (i.y + i.bh), o.y - i.y, o.y + o.bh - (i.y + i.bh)];
    // Side by side: snap x to touch, align y if close.
    for (const dx of xs.slice(0, 2)) {
      const overlapY = Math.min(i.y + i.bh, o.y + o.bh) - Math.max(i.y, o.y);
      if (Math.abs(dx) < t * 4 && overlapY > -t) {
        const dy = ys.slice(2).reduce((a, b) => (Math.abs(b) < Math.abs(a) ? b : a), Infinity);
        const cand = { dx, dy: Math.abs(dy) < t ? dy : overlapY > 0 ? 0 : ys[2], d: Math.abs(dx) };
        if (!best || cand.d < best.d) best = cand;
      }
    }
    // Stacked: snap y to touch, align x if close.
    for (const dy of ys.slice(0, 2)) {
      const overlapX = Math.min(i.x + i.bw, o.x + o.bw) - Math.max(i.x, o.x);
      if (Math.abs(dy) < t * 4 && overlapX > -t) {
        const dx = xs.slice(2).reduce((a, b) => (Math.abs(b) < Math.abs(a) ? b : a), Infinity);
        const cand = { dx: Math.abs(dx) < t ? dx : overlapX > 0 ? 0 : xs[2], dy, d: Math.abs(dy) };
        if (!best || cand.d < best.d) best = cand;
      }
    }
  }
  if (best) {
    i.x += Math.round(best.dx);
    i.y += Math.round(best.dy);
  }
}

function touching(a: Item, b: Item) {
  const tol = 8;
  const overlapY = Math.min(a.y + a.bh, b.y + b.bh) - Math.max(a.y, b.y);
  const overlapX = Math.min(a.x + a.bw, b.x + b.bw) - Math.max(a.x, b.x);
  const sideBySide = overlapY > 0 && (Math.abs(a.x + a.bw - b.x) <= tol || Math.abs(b.x + b.bw - a.x) <= tol);
  const stacked = overlapX > 0 && (Math.abs(a.y + a.bh - b.y) <= tol || Math.abs(b.y + b.bh - a.y) <= tol);
  return sideBySide || stacked;
}

const isolated = computed(() =>
  items.value.length > 1 ? items.value.filter((a) => !items.value.some((b) => b.id !== a.id && touching(a, b))) : [],
);

function autoArrange() {
  let x = 0;
  const ordered = [...items.value].sort((a, b) => Number(b.isLocal) - Number(a.isLocal));
  for (const i of ordered) {
    i.x = x;
    i.y = 0;
    x += i.bw;
  }
  dirty.value = true;
  fit();
}

async function save() {
  const ok = await act(() => api.saveLayout(items.value.map((i) => ({ id: i.id, x: i.x, y: i.y }))), "Layout saved and shared with connected PCs");
  if (ok) dirty.value = false;
}

watch(
  () => state.layoutRevision,
  () => {
    if (!dirty.value && !drag.id) void load();
  },
);

let ro: ResizeObserver | undefined;
onMounted(() => {
  void load();
  ro = new ResizeObserver(() => !drag.id && fit());
  if (canvas.value) ro.observe(canvas.value);
});
onBeforeUnmount(() => ro?.disconnect());
</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold">Monitor layout</h1>
        <p class="mt-1 text-sm muted">
          Drag each computer to where it physically sits. The cursor crosses to another computer wherever their edges touch.
        </p>
      </div>
      <div class="flex gap-2">
        <button class="btn btn-ghost" @click="autoArrange">Auto arrange</button>
        <button class="btn btn-secondary" :disabled="!dirty" @click="load">Revert</button>
        <button class="btn btn-primary" :disabled="!dirty" @click="save">Save layout</button>
      </div>
    </header>

    <div
      ref="canvas"
      class="relative overflow-hidden rounded-xl border border-slate-200 bg-[radial-gradient(circle,_rgb(148_163_184_/_0.25)_1px,_transparent_1px)] [background-size:20px_20px] dark:border-slate-800"
      :style="{ height: `${canvasHeight}px` }"
      @pointermove="drag.id && onPointerMove($event)"
      @pointerup="onPointerUp"
    >
      <div
        v-for="i in items"
        :key="i.id"
        class="absolute cursor-grab touch-none rounded-md transition-shadow active:cursor-grabbing"
        :class="[drag.id === i.id ? 'z-10 shadow-2xl' : 'shadow-md', drag.id === i.id ? '' : 'transition-[left,top] duration-150']"
        :style="boxStyle(i)"
        @pointerdown="onPointerDown($event, i)"
      >
        <div
          v-for="(s, idx) in i.screens"
          :key="idx"
          class="absolute rounded-[3px] border-2"
          :class="
            i.isLocal
              ? 'border-indigo-500 bg-indigo-500/20 dark:bg-indigo-500/25'
              : i.online
                ? 'border-sky-500 bg-sky-500/15'
                : 'border-dashed border-slate-400 bg-slate-400/10'
          "
          :style="monitorStyle(i, s)"
        />
        <div class="pointer-events-none absolute inset-0 flex flex-col items-center justify-center p-1 text-center">
          <span class="max-w-full truncate rounded bg-white/90 px-1.5 text-xs font-semibold text-slate-800 shadow-sm dark:bg-slate-900/90 dark:text-slate-100">
            {{ i.name }}
          </span>
          <span class="mt-0.5 text-[10px] font-medium text-slate-600 dark:text-slate-300">
            {{ i.isLocal ? "This PC" : i.online ? "Online" : "Offline" }} · {{ i.screens.length }}
            {{ i.screens.length === 1 ? "display" : "displays" }}
          </span>
        </div>
      </div>
    </div>

    <div
      v-if="isolated.length"
      class="rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200"
    >
      {{ isolated.map((i) => i.name).join(", ") }} {{ isolated.length === 1 ? "does" : "do" }} not touch any other computer, so the
      cursor cannot reach {{ isolated.length === 1 ? "it" : "them" }}. Drop it next to another computer; it will snap into place.
    </div>

    <div class="grid gap-4 text-sm sm:grid-cols-3">
      <div class="card p-4">
        <div class="font-medium">Different resolutions</div>
        <p class="mt-1 text-xs muted">Sizes are drawn in real pixels. Where edges only partly overlap, the cursor crosses within the overlap.</p>
      </div>
      <div class="card p-4">
        <div class="font-medium">Shared arrangement</div>
        <p class="mt-1 text-xs muted">Saving sends the layout to every connected PC, so all of them agree on where each screen is.</p>
      </div>
      <div class="card p-4">
        <div class="font-medium">Emergency return</div>
        <p class="mt-1 text-xs muted">Press <kbd class="font-mono">Ctrl+Alt+Home</kbd> to bring control back to the PC whose keyboard you're using.</p>
      </div>
    </div>
  </div>
</template>
