import { useState } from 'react';

export const stateLabels = {
  running: 'En cours', shutoff: 'Arrêtée', shutdown: 'Extinction',
  paused: 'En pause', blocked: 'Bloquée', crashed: 'Crashée',
  pmsuspended: 'Suspendue', unknown: 'Inconnu',
};
export const roleLabels = { viewer: 'Lecteur', operator: 'Opérateur', admin: 'Administrateur' };

export function StateBadge({ state }) {
  return <span className={`state-badge state-${state}`}><span className="state-dot" />{stateLabels[state] || state}</span>;
}
export function Metric({ label, value, hint }) {
  return <article className="metric"><div className="metric-label">{label}</div><div className="metric-value">{value}</div><div className="metric-hint">{hint}</div></article>;
}
export function Modal({ title, children, onClose, wide }) {
  return <div className="modal-scrim" role="presentation">
    <div className={`modal-panel ${wide ? 'wide-modal' : ''}`} role="dialog" aria-modal="true" aria-label={title}>
      <div className="modal-top"><h2>{title}</h2><button className="close-button" type="button" onClick={onClose} aria-label="Fermer">×</button></div>
      {children}
    </div>
  </div>;
}
export function Banner({ error, notice, onClear }) {
  return <>
    {error && <div className="error-panel" role="alert"><span>{error}</span>{onClear && <button type="button" onClick={() => onClear('error')} aria-label="Masquer l'erreur">×</button>}</div>}
    {notice && <div className="notice-panel" role="status"><span>✓ {notice}</span>{onClear && <button type="button" onClick={() => onClear('notice')} aria-label="Masquer le message">×</button>}</div>}
  </>;
}
export function PageHeader({ eyebrow, title, subtitle, children }) {
  return <div className="heading-row"><div>
    <div className="eyebrow">{eyebrow}</div><h1>{title}</h1><p className="subtitle">{subtitle}</p>
  </div><div className="heading-actions">{children}</div></div>;
}
// Petit hook : exécute une action asynchrone avec état « en cours » + messages.
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const clear = (kind) => (kind === 'error' ? setError('') : setNotice(''));
  async function run(fn, { confirm } = {}) {
    if (busy) return false;
    if (confirm && !window.confirm(confirm)) return false;
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await fn();
      if (result?.message) setNotice(result.message);
      return true;
    } catch (e) { setError(e.message); return false; } finally { setBusy(false); }
  }
  return { busy, error, notice, setError, setNotice, clear, run };
}
