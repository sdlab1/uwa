<script lang="ts">
  let {
    acquisition = 0,
    send = 0,
    wait = 0,
    height = 14,
    showLegend = false,
  }: {
    acquisition?: number;
    send?: number;
    wait?: number;
    height?: number;
    showLegend?: boolean;
  } = $props();

  const sum = $derived(Math.max(1, acquisition + send + wait));
  const acqPct  = $derived((acquisition / sum) * 100);
  const sendPct = $derived((send / sum) * 100);
  const waitPct = $derived((wait / sum) * 100);
</script>

<div class="wf">
  <div class="wf-bar" style="height: {height}px"
       title={`acquisition ${Math.round(acquisition)} ms · send ${Math.round(send)} ms · wait ${Math.round(wait)} ms`}>
    {#if acqPct  > 0}<div class="wf-seg wf-acq"  style="width: {acqPct}%"></div>{/if}
    {#if sendPct > 0}<div class="wf-seg wf-send" style="width: {sendPct}%"></div>{/if}
    {#if waitPct > 0}<div class="wf-seg wf-wait" style="width: {waitPct}%"></div>{/if}
  </div>
  {#if showLegend}
    <div class="wf-legend">
      <span><span class="wf-dot wf-acq"></span>acquisition</span>
      <span><span class="wf-dot wf-send"></span>send</span>
      <span><span class="wf-dot wf-wait"></span>wait</span>
    </div>
  {/if}
</div>
