<script lang="ts">
  let { title, message, hint, confirmLabel = "Stop & continue", cancelLabel = "Cancel", onConfirm, onCancel } = $props<{
    title: string;
    message: string;
    /** Optional secondary line shown in a muted box below the message —
     *  use it for "in-flight inference will finish first" style notes. */
    hint?: string;
    confirmLabel?: string;
    cancelLabel?: string;
    onConfirm: () => void;
    onCancel: () => void;
  }>();
</script>

<div class="overlay" onclick={onCancel} role="presentation"></div>
<div class="modal" role="dialog" aria-label={title}>
  <h3>{title}</h3>
  <p class="msg">{message}</p>
  {#if hint}
    <p class="hint">{hint}</p>
  {/if}
  <div class="actions">
    <button class="cancel" onclick={onCancel}>{cancelLabel}</button>
    <button class="confirm" onclick={onConfirm}>{confirmLabel}</button>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, .55);
    z-index: 9000;
  }
  .modal {
    position: fixed;
    top: 50%; left: 50%;
    transform: translate(-50%, -50%);
    width: min(420px, 90vw);
    background: #161616;
    border: 1px solid #2a2a2a;
    border-radius: 10px;
    padding: 1.1rem 1.2rem;
    z-index: 9001;
    box-shadow: 0 18px 48px rgba(0, 0, 0, .65);
  }
  h3 {
    font-size: .95rem;
    font-weight: 600;
    color: #e8e8e8;
    margin-bottom: .65rem;
  }
  .msg {
    font-size: .82rem;
    color: #ccc;
    line-height: 1.55;
    margin-bottom: .55rem;
  }
  .hint {
    font-size: .76rem;
    color: #888;
    line-height: 1.5;
    background: #131318;
    padding: .5rem .65rem;
    border-radius: 6px;
    margin-bottom: .85rem;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: .55rem;
    margin-top: .25rem;
  }
  .actions button {
    padding: .45rem 1rem;
    border-radius: 6px;
    font-size: .82rem;
    cursor: pointer;
    border: 1px solid transparent;
  }
  .actions .cancel {
    background: #1e1e1e;
    color: #ccc;
    border-color: #2a2a2a;
  }
  .actions .cancel:hover { background: #252525; }
  .actions .confirm {
    background: #5a2424;
    color: #ffd6d6;
    border-color: #7a3434;
  }
  .actions .confirm:hover { background: #6a2c2c; }
</style>
