import { useCallback, useEffect, useState } from 'react';
import { api, getHv, hasRole, jsonOptions, onUnauthorized, setHv } from './api.js';
import { Banner, Modal, roleLabels, useAction } from './common.jsx';
import Login from './pages/Login.jsx';
import Vms from './pages/Vms.jsx';
import Storage from './pages/Storage.jsx';
import Templates from './pages/Templates.jsx';
import { Audit, Hypervisors, Users } from './pages/Admin.jsx';

const PAGES = [
  { key: 'vms', label: 'Machines virtuelles', icon: '▦', min: 'viewer' },
  { key: 'storage', label: 'Stockage & ISO', icon: '◫', min: 'viewer' },
  { key: 'templates', label: 'Modèles IaC', icon: '⌘', min: 'viewer' },
  { key: 'hypervisors', label: 'Hyperviseurs', icon: '⬡', min: 'admin' },
  { key: 'users', label: 'Utilisateurs', icon: '☺', min: 'admin' },
  { key: 'audit', label: "Journal d'audit", icon: '☰', min: 'admin' },
];

function PasswordModal({ onClose }) {
  const [form, setForm] = useState({ current: '', next: '', again: '' });
  const act = useAction();
  const set = (k) => (e) => setForm((f) => ({ ...f, [k]: e.target.value }));
  const mismatch = form.again !== '' && form.next !== form.again;
  async function submit(e) {
    e.preventDefault();
    if (mismatch) return;
    const ok = await act.run(() => api('/api/v1/auth/password', jsonOptions('POST', { current_password: form.current, new_password: form.next })));
    if (ok) setForm({ current: '', next: '', again: '' });
  }
  return <Modal title="Changer mon mot de passe" onClose={onClose}>
    <form className="modal-body" onSubmit={submit}>
      <Banner error={act.error} notice={act.notice} />
      <label className="field">Mot de passe actuel<input required type="password" autoComplete="current-password" value={form.current} onChange={set('current')} /></label>
      <label className="field">Nouveau mot de passe (10 caractères minimum)<input required minLength={10} type="password" autoComplete="new-password" value={form.next} onChange={set('next')} /></label>
      <label className="field">Confirmer<input required type="password" autoComplete="new-password" value={form.again} onChange={set('again')} /></label>
      {mismatch && <div className="error-panel">Les deux mots de passe diffèrent.</div>}
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Fermer</button><button type="submit" className="refresh-button" disabled={act.busy || mismatch}>Modifier</button></div>
    </form>
  </Modal>;
}

function Shell({ user, onLogout }) {
  const allowed = PAGES.filter((p) => hasRole(user, p.min));
  const fromHash = () => { const k = window.location.hash.slice(1); return allowed.some((p) => p.key === k) ? k : 'vms'; };
  const [page, setPage] = useState(fromHash);
  const [hypervisors, setHypervisors] = useState([]);
  const [hv, setHvState] = useState(getHv());
  const [showPassword, setShowPassword] = useState(false);

  useEffect(() => {
    const onHash = () => setPage(fromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const loadHypervisors = useCallback(async () => {
    try {
      const list = await api('/api/v1/hypervisors');
      setHypervisors(list);
      if (!list.some((h) => h.id === getHv())) { setHv('local'); setHvState('local'); }
    } catch { /* l'erreur s'affichera dans les pages */ }
  }, []);
  useEffect(() => { loadHypervisors(); }, [loadHypervisors, page]);

  const go = (key) => { window.location.hash = key; setPage(key); };
  const changeHv = (id) => { setHv(id); setHvState(id); };
  const current = hypervisors.find((h) => h.id === hv);

  return <div className="layout">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">V</span><span>Virt<span className="brand-light">Manager</span></span></div>
      <div className="side-label">HYPERVISEUR</div>
      <select className="hv-select" aria-label="Hyperviseur actif" value={hv} onChange={(e) => changeHv(e.target.value)}>
        {(hypervisors.length ? hypervisors : [{ id: 'local', name: 'Hyperviseur local' }]).map((h) => <option key={h.id} value={h.id}>{h.name}</option>)}
      </select>
      <div className="side-label second-label">ESPACE DE TRAVAIL</div>
      <nav className="side-nav">
        {allowed.map((p) => <button type="button" key={p.key} className={`nav-item ${page === p.key ? 'nav-active' : ''}`} onClick={() => go(p.key)}><span>{p.icon}</span> {p.label}</button>)}
      </nav>
      <div className="user-box">
        <div><strong>{user.username}</strong><span>{roleLabels[user.role] || user.role}</span></div>
        <button type="button" className="link-button" onClick={() => setShowPassword(true)}>Mot de passe</button>
        <button type="button" className="link-button" onClick={onLogout}>Se déconnecter</button>
      </div>
      <div className="sidebar-footer"><span className="footer-indicator" /> v1.0.0 · {current?.kind === 'ssh' ? 'hôte distant (SSH)' : current?.kind === 'tls' ? 'hôte distant (TLS)' : 'libvirt local'}</div>
    </aside>
    <main className="main">
      <header className="topbar"><span>Administration / {PAGES.find((p) => p.key === page)?.label}</span><a href="/api-docs/openapi.json" target="_blank" rel="noreferrer">Documentation API ↗</a></header>
      <div className="content" key={`${page}:${hv}`}>
        {page === 'vms' && <Vms user={user} />}
        {page === 'storage' && <Storage user={user} />}
        {page === 'templates' && <Templates />}
        {page === 'hypervisors' && <Hypervisors onChanged={loadHypervisors} />}
        {page === 'users' && <Users me={user} />}
        {page === 'audit' && <Audit />}
      </div>
    </main>
    {showPassword && <PasswordModal onClose={() => setShowPassword(false)} />}
  </div>;
}

export default function App() {
  const [user, setUser] = useState(undefined); // undefined = vérification en cours, null = déconnecté
  useEffect(() => {
    onUnauthorized(() => setUser(null));
    api('/api/v1/auth/me').then(setUser).catch(() => setUser(null));
  }, []);
  async function logout() {
    try { await api('/api/v1/auth/logout', jsonOptions('POST', {})); } catch { /* session déjà fermée */ }
    setUser(null);
  }
  if (user === undefined) return <div className="login-screen"><p className="subtitle">Chargement…</p></div>;
  if (user === null) return <Login onLogin={setUser} />;
  return <Shell user={user} onLogout={logout} />;
}
