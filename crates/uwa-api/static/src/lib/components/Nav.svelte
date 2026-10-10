<script lang="ts">
  import { route } from '../router';

  const items = [
    { path: '/setup',      label: 'Setup' },
    { path: '/',           label: 'Dashboard' },
    { path: '/playground', label: 'Playground' },
    { path: '/history',    label: 'History' },
    { path: '/sessions',   label: 'Sessions' },
    { path: '/config',     label: 'Config' },
    { path: '/logs',       label: 'Logs' },
    { path: '/debug',      label: 'Debug' },
  ];
</script>

<nav class="flex gap-1 px-5 py-2 border-b border-border bg-panel overflow-x-auto">
  {#each items as it (it.path)}
    <a href={`#${it.path}`}
       class="px-3 py-1.5 rounded-sm text-[13px] whitespace-nowrap no-underline"
       class:bg-panel3={$route === it.path}
       class:text-text={$route === it.path}
       class:text-muted={$route !== it.path}
       class:hover:text-text={$route !== it.path}
       class:hover:bg-panel2={$route !== it.path}>
      {it.label}
    </a>
  {/each}
</nav>
