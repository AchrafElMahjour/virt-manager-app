import { useCallback, useEffect, useState } from 'react';
import { api, jsonOptions } from '../api.js';
import { Banner, Modal, PageHeader, roleLabels, useAction } from '../common.jsx';

// ───────────────────────── Hyperviseurs ─────────────────────────
function AddHypervisorModal({ act, onClose, onSubmit }) {
  const [form, setForm] = useState({ name: '', uri: 'qemu+ssh://utilisateur@hote/system' });
  return <Modal title="Ajouter un hyperviseur distant" onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit({ name: form.name.trim(), uri: form.uri.trim() }); }}>
      <Banner error={act.error} />
      <label className="field">Nom affiché<input required maxLength={64} placeholder="Labo 1" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></label>
      <label className="field">URI libvirt<input required spellCheck={false} value={form.uri} onChange={(e) => setForm({ ...form, uri: e.target.value })} /></label>
      <p className="form-tip">Formes acceptées : <code>qemu+ssh://user@hote[:port]/system[?keyfile=/chemin/cle]</code> ou <code>qemu+tls://hote/system</code>. L'authentification doit être non interactive (clé SSH sans mot de passe, hôte déjà présent dans known_hosts). Les paramètres pouvant exécuter des commandes sont refusés. Le serveur Virt Manager se connecte avec <em>son</em> utilisateur système.</p>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={act.busy}>Ajouter</button></div>
    </form>
  </Modal>;
}

export function Hypervisors({ onChanged }) {
  const [list, setList] = useState([]);
  const [results, setResults] = useState({});
  const [modal, setModal] = useState(false);
  const act = useAction();
  const load = useCallback(async () => { try { setList(await api('/api/v1/hypervisors')); } catch (e) { act.setError(e.message); } }, []); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { load(); }, [load]);
  const test = (h) => act.run(async () => {
    setResults((r) => ({ ...r, [h.id]: { pending: true } }));
    const res = await api(`/api/v1/hypervisors/${encodeURIComponent(h.id)}/test`, { ...jsonOptions('POST', {}), hv: h.id });
    setResults((r) => ({ ...r, [h.id]: res }));
  });
  return <>
    <PageHeader eyebrow="ADMINISTRATION" title="Hyperviseurs" subtitle="Hôtes libvirt gérés par cette instance. Choisissez l'hyperviseur actif dans la barre latérale.">
      <button type="button" className="refresh-button" onClick={() => { act.setError(''); setModal(true); }}>+ Ajouter un hôte distant</button>
    </PageHeader>
    <Banner error={!modal && act.error} notice={act.notice} onClear={(k) => (k === 'error' ? act.setError('') : act.setNotice(''))} />
    <section className="table-card"><div className="table-scroll"><table><thead><tr><th>Nom</th><th>Type</th><th>URI</th><th>Test de connexion</th><th>Actions</th></tr></thead><tbody>
      {list.map((h) => { const r = results[h.id]; return <tr key={h.id}>
        <td><strong>{h.name}</strong><span className="uuid">{h.id}</span></td>
        <td>{h.kind === 'local' ? 'Local' : h.kind === 'ssh' ? 'Distant (SSH)' : 'Distant (TLS)'}</td>
        <td><code>{h.uri}</code></td>
        <td>{!r ? <span className="muted">—</span> : r.pending ? 'Test en cours…' : r.ok
          ? <span className="ok-text">✓ {r.hostname} · {r.hypervisor_type} · {r.domains} VM</span>
          : <span className="err-text" title={r.error}>✗ {(r.error || '').slice(0, 90)}</span>}</td>
        <td><div className="actions">
          <button type="button" className="vm-action neutral" disabled={act.busy} onClick={() => test(h)}>Tester</button>
          {!h.builtin && <button type="button" className="vm-action danger" disabled={act.busy} onClick={() => act.run(async () => { const m = await api(`/api/v1/hypervisors/${encodeURIComponent(h.id)}`, { method: 'DELETE' }); await load(); onChanged(); return m; }, { confirm: `Retirer « ${h.name} » de la liste ? Aucune VM n'est modifiée.` })}>Retirer</button>}
        </div></td>
      </tr>; })}
    </tbody></table></div></section>
    <p className="read-only-note">Console web : disponible pour les hôtes locaux et <code>qemu+ssh://</code> (tunnel SSH, rien n'est exposé sur le réseau). Pour <code>qemu+tls://</code>, utilisez virt-manager.</p>
    {modal && <AddHypervisorModal act={act} onClose={() => setModal(false)} onSubmit={async (body) => { if (await act.run(async () => { await api('/api/v1/hypervisors', jsonOptions('POST', body)); await load(); onChanged(); return { message: `Hyperviseur « ${body.name} » ajouté.` }; })) setModal(false); }} />}
  </>;
}

// ───────────────────────── Utilisateurs ─────────────────────────
function UserModal({ title, user, act, onClose, onSubmit }) {
  const editing = Boolean(user);
  const [form, setForm] = useState({ username: user?.username ?? '', password: '', role: user?.role ?? 'viewer', disabled: user?.disabled ?? false });
  return <Modal title={title} onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit(form); }}>
      <Banner error={act.error} />
      <label className="field">Identifiant<input required disabled={editing} pattern="[a-z0-9][a-z0-9._-]{2,31}" placeholder="prenom.nom" value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value.toLowerCase() })} /></label>
      <label className="field">{editing ? 'Nouveau mot de passe (laisser vide pour ne pas changer)' : 'Mot de passe (10 caractères minimum)'}<input type="password" autoComplete="new-password" minLength={form.password ? 10 : undefined} required={!editing} value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} /></label>
      <label className="field">Rôle<select value={form.role} onChange={(e) => setForm({ ...form, role: e.target.value })}>
        <option value="viewer">Lecteur — consultation seule</option>
        <option value="operator">Opérateur — + démarrer/arrêter/pause et console</option>
        <option value="admin">Administrateur — tous les droits</option>
      </select></label>
      {editing && <label className="checkbox-row"><input type="checkbox" checked={form.disabled} onChange={(e) => setForm({ ...form, disabled: e.target.checked })} /> Compte désactivé (connexion impossible)</label>}
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={act.busy}>Enregistrer</button></div>
    </form>
  </Modal>;
}

export function Users({ me }) {
  const [users, setUsers] = useState([]);
  const [modal, setModal] = useState(null);
  const act = useAction();
  const load = useCallback(async () => { try { setUsers(await api('/api/v1/users')); } catch (e) { act.setError(e.message); } }, []); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { load(); }, [load]);
  const save = async (form) => {
    const editing = modal.user;
    const ok = await act.run(async () => {
      if (editing) {
        const body = { role: form.role, disabled: form.disabled };
        if (editing.username === me.username) { delete body.role; delete body.disabled; }
        if (form.password) body.password = form.password;
        await api(`/api/v1/users/${encodeURIComponent(editing.username)}`, jsonOptions('PUT', body));
      } else {
        await api('/api/v1/users', jsonOptions('POST', { username: form.username, password: form.password, role: form.role }));
      }
      await load();
      return { message: `Compte « ${editing?.username || form.username} » enregistré.` };
    });
    if (ok) setModal(null);
  };
  return <>
    <PageHeader eyebrow="ADMINISTRATION" title="Utilisateurs" subtitle="Comptes et rôles. Un changement de rôle, de mot de passe ou une désactivation ferme les sessions du compte.">
      <button type="button" className="refresh-button" onClick={() => { act.setError(''); setModal({ user: null }); }}>+ Nouveau compte</button>
    </PageHeader>
    <Banner error={!modal && act.error} notice={act.notice} onClear={(k) => (k === 'error' ? act.setError('') : act.setNotice(''))} />
    <section className="table-card"><div className="table-scroll"><table><thead><tr><th>Identifiant</th><th>Rôle</th><th>État</th><th>Créé le</th><th>Actions</th></tr></thead><tbody>
      {users.map((u) => <tr key={u.username}>
        <td><strong>{u.username}</strong>{u.username === me.username && <span className="managed-label">vous</span>}</td>
        <td>{roleLabels[u.role]}</td>
        <td>{u.disabled ? <span className="err-text">Désactivé</span> : <span className="ok-text">Actif</span>}</td>
        <td>{u.created_at ? new Date(u.created_at * 1000).toLocaleDateString('fr-FR') : '—'}</td>
        <td><div className="actions">
          <button type="button" className="vm-action neutral" disabled={act.busy} onClick={() => { act.setError(''); setModal({ user: u }); }}>Modifier</button>
          {u.username !== me.username && <button type="button" className="vm-action danger" disabled={act.busy} onClick={() => act.run(async () => { const m = await api(`/api/v1/users/${encodeURIComponent(u.username)}`, { method: 'DELETE' }); await load(); return m; }, { confirm: `Supprimer le compte « ${u.username} » ?` })}>Supprimer</button>}
        </div></td>
      </tr>)}
    </tbody></table></div></section>
    <div className="role-help">
      <div><strong>Lecteur</strong> consulte VM, stockage, modèles.</div>
      <div><strong>Opérateur</strong> peut en plus démarrer, mettre en pause, éteindre et ouvrir la console.</div>
      <div><strong>Administrateur</strong> crée, clone, supprime, configure le stockage, les hôtes et les comptes.</div>
    </div>
    {modal && <UserModal title={modal.user ? `Modifier « ${modal.user.username} »` : 'Nouveau compte'} user={modal.user} act={act} onClose={() => setModal(null)} onSubmit={save} />}
  </>;
}

// ───────────────────────── Audit ─────────────────────────
export function Audit() {
  const [entries, setEntries] = useState([]);
  const [error, setError] = useState('');
  const load = useCallback(async () => { try { setEntries(await api('/api/v1/audit?limit=300')); setError(''); } catch (e) { setError(e.message); } }, []);
  useEffect(() => { load(); }, [load]);
  return <>
    <PageHeader eyebrow="SÉCURITÉ" title="Journal d'audit" subtitle="Connexions et actions modifiantes (jamais de mots de passe ni de contenu de requête). Les 300 plus récentes.">
      <button type="button" className="secondary-button" onClick={load}>↻ Actualiser</button>
    </PageHeader>
    {error && <div className="error-panel" role="alert"><span>{error}</span></div>}
    <section className="table-card"><div className="table-scroll"><table><thead><tr><th>Date (UTC)</th><th>Utilisateur</th><th>IP</th><th>Requête</th><th>Hôte</th><th>Statut</th></tr></thead><tbody>
      {entries.map((e, i) => <tr key={i}>
        <td>{e.ts.replace('T', ' ').replace('Z', '')}</td><td>{e.user}</td><td>{e.ip}</td>
        <td><code>{e.method} {e.path}</code></td><td>{e.hypervisor || '—'}</td>
        <td><span className={e.status < 300 ? 'ok-text' : 'err-text'}>{e.status}</span></td>
      </tr>)}
      {!entries.length && <tr><td className="empty" colSpan={6}>Aucune entrée.</td></tr>}
    </tbody></table></div></section>
  </>;
}
