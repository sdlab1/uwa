import { writable } from 'svelte/store';

function currentPath(): string {
  const h = location.hash.replace(/^#/, '');
  return h || '/';
}

export const route = writable<string>(currentPath());

window.addEventListener('hashchange', () => route.set(currentPath()));

export function navigate(path: string): void {
  if (location.hash === `#${path}`) return;
  location.hash = path;
}
