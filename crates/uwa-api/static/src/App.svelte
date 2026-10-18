<script lang="ts">
  import { route } from './lib/router';
  import Topbar from './lib/components/Topbar.svelte';
  import Nav from './lib/components/Nav.svelte';
  import Toasts from './lib/components/Toasts.svelte';

  import Dashboard  from './routes/Dashboard.svelte';
  import Setup      from './routes/Setup.svelte';
  import Playground from './routes/Playground.svelte';
  import Clients    from './routes/Clients.svelte';
  import Profiles   from './routes/Profiles.svelte';
  import Recorder   from './routes/Recorder.svelte';
  import History    from './routes/History.svelte';
  import Sessions   from './routes/Sessions.svelte';
  import Verification from './routes/Verification.svelte';
  import Config     from './routes/Config.svelte';
  import Logs       from './routes/Logs.svelte';
  import Events     from './routes/Events.svelte';
  import Debug      from './routes/Debug.svelte';
  import Conformance from './routes/Conformance.svelte';

  const routes: Record<string, any> = {
    '/': Dashboard,
    '/setup': Setup,
    '/playground': Playground,
    '/clients': Clients,
    '/profiles': Profiles,
    '/recorder': Recorder,
    '/history': History,
    '/sessions': Sessions,
    '/verification': Verification,
    '/config': Config,
    '/logs': Logs,
    '/events': Events,
    '/debug': Debug,
    '/conformance': Conformance,
  };

  let path = $state('/');
  route.subscribe((v) => { path = v; });

  // First-run: if setup hasn't been completed, go there.
  $effect(() => {
    if (!localStorage.getItem('uwa:setup:done') && !location.hash) {
      location.hash = '/setup';
    }
  });

  let View = $derived(routes[path] ?? Dashboard);
</script>

<Topbar />
<Nav />

<main class="max-w-[1100px] mx-auto px-5 pt-6 pb-20">
  {#key path}
    <View />
  {/key}
</main>

<Toasts />
