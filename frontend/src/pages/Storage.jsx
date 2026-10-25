import { useCallback, useEffect, useState } from 'react';
import { api, formatBytes, hasRole, jsonOptions, poolPath, uploadIso } from '../api.js';
import { Banner, Modal, PageHeader, useAction } from '../common.jsx';

const sanitizeIsoName = (name) => name.replace(/[^A-Za-z0-9._-]/g, '-').replace(/^[.-]+/, '');

function UsageBar({ pool }) {
  if (!pool.active || !pool.capacity_bytes) return <span className="muted">—</span>;
  const pct = Math.min(100, Math.round((pool.allocation_bytes / pool.capacity_bytes) * 100));
  return <div className="usage"><div className="usage-bar"><span style={{ width: `${pct}%` }} className={pct > 90 ? 'usage-hot' : ''} /></div>
    <small>{formatBytes(pool.available_bytes)} libres sur {formatBytes(pool.capacity_bytes)}</small></div>;
}

function CreatePoolModal({ act, onClose, onSubmit }) {
  const [form, setForm] = useState({ name: '', path: '/var/lib/libvirt/' });
  return <Modal title="Créer un pool de stockage (dir)" onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit(form); }}>
      <Banner error={act.error} />
      <label className="field">Nom du pool<input required pattern="[A-Za-z0-9][A-Za-z0-9._-]*" maxLength={64} placeholder="isos" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></label>
      <label className="field">Répertoire (absolu)<input required placeholder="/var/lib/libvirt/isos" value={form.path} onChange={(e) => setForm({ ...form, path: e.target.value })} /></label>
      <p className="form-tip">Le répertoire doit se trouver sous une racine autorisée par le serveur (<code>VIRTMGR_POOL_ROOTS</code>, par défaut /var/lib/libvirt, /srv, /mnt, /media, /opt, /data, /home). Il est créé s'il n'existe pas, puis le pool est démarré et placé en démarrage automatique.</p>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={act.busy}>{act.busy ? 'Création…' : 'Créer le pool'}</button></div>
    </form>
  </Modal>;
}

function CreateVolumeModal({ pool, act, onClose, onSubmit }) {
  const [form, setForm] = useState({ name: '', size_gib: 10, format: 'qcow2' });
  return <Modal title={`Nouveau volume dans « ${pool} »`} onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit({ ...form, size_gib: Number(form.size_gib) }); }}>
      <Banner error={act.error} />
      <label className="field">Nom<input required pattern="[A-Za-z0-9][A-Za-z0-9._-]*" maxLength={90} placeholder="donnees-01" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></label>
      <div className="field-grid">
        <label className="field">Taille (Gio)<input required type="number" min="1" max="2048" value={form.size_gib} onChange={(e) => setForm({ ...form, size_gib: e.target.value })} /></label>
        <label className="field">Format<select value={form.format} onChange={(e) => setForm({ ...form, format: e.target.value })}><option value="qcow2">qcow2 (allocation à la demande)</option><option value="raw">raw</option></select></label>
      </div>
      <p className="form-tip">Volume vierge, non attaché. L'extension (.qcow2 / .img) est ajoutée au besoin. Le préfixe « virtmgr- » est réservé aux disques des VM gérées.</p>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={act.busy}>Créer</button></div>
    </form>
  </Modal>;
}

function ConfirmDelete({ title, name, warning, act, onClose, onSubmit }) {
  const [typed, setTyped] = useState('');
  return <Modal title={title} onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit(typed); }}>
      <Banner error={act.error} />
      <p className="delete-warning">{warning}</p>
      <label className="field">Tapez exactement <strong>{name}</strong> pour confirmer<input required autoComplete="off" value={typed} onChange={(e) => setTyped(e.target.value)} /></label>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="delete-button" disabled={act.busy || typed !== name}>Confirmer la suppression</button></div>
    </form>
  </Modal>;
}

function IsoUpload({ pool, existing, onDone }) {
  const [file, setFile] = useState(null);
  const [name, setName] = useState('');
  const [progress, setProgress] = useState(null);
  const act = useAction();
  const pick = (f) => { setFile(f); setName(f ? sanitizeIsoName(f.name) : ''); };
  async function send(e) {
    e.preventDefault();
    if (existing.includes(name)) { act.setError(`Un volume « ${name} » existe déjà dans ce pool : choisissez un autre nom.`); return; }
    setProgress(0);
    const ok = await act.run(() => uploadIso(pool, name, file, setProgress));
    setProgress(null);
    if (ok) { setFile(null); setName(''); onDone(); }
  }
  const nameOk = /^[A-Za-z0-9][A-Za-z0-9._-]*\.iso$/i.test(name) && !name.startsWith('virtmgr-');
  return <form className="upload-form" onSubmit={send}>
    <h3>Téléverser une ISO dans « {pool} »</h3>
    <Banner error={act.error} notice={act.notice} onClear={(k) => (k === 'error' ? act.setError('') : act.setNotice(''))} />
    <div className="upload-row">
      <input type="file" accept=".iso" aria-label="Fichier ISO" onChange={(e) => pick(e.target.files?.[0] ?? null)} disabled={act.busy} />
      <input aria-label="Nom de l'ISO sur le serveur" placeholder="nom.iso" value={name} onChange={(e) => setName(e.target.value)} disabled={act.busy} />
      <button type="submit" className="refresh-button" disabled={!file || !nameOk || act.busy}>{act.busy ? 'Envoi…' : 'Envoyer'}</button>
    </div>
    {file && !nameOk && <small className="muted">Nom attendu : lettres, chiffres, « . _ - », extension .iso.</small>}
    {progress !== null && <div className="usage"><div className="usage-bar"><span style={{ width: `${Math.round(progress * 100)}%` }} /></div><small>{Math.round(progress * 100)} % — ne fermez pas cette page pendant l'envoi</small></div>}
  </form>;
}

export default function Storage({ user }) {
  const admin = hasRole(user, 'admin');
  const [pools, setPools] = useState([]);
  const [volumes, setVolumes] = useState([]);
  const [selected, setSelected] = useState('');
  const [loadError, setLoadError] = useState('');
  const [modal, setModal] = useState(null);
  const act = useAction();

  const loadPools = useCallback(async () => {
    try { setPools(await api('/api/v1/storage/pools')); setLoadError(''); } catch (e) { setLoadError(e.message); }
  }, []);
  const loadVolumes = useCallback(async (name) => {
    if (!name) { setVolumes([]); return; }
    try { setVolumes(await api(`${poolPath(name)}/volumes`)); } catch (e) { setVolumes([]); act.setError(e.message); }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { loadPools(); const t = setInterval(loadPools, 15000); return () => clearInterval(t); }, [loadPools]);
  useEffect(() => { loadVolumes(selected); }, [selected, loadVolumes]);
  useEffect(() => {
    if (!selected && pools.length) setSelected((pools.find((p) => p.name === 'default' && p.active) || pools.find((p) => p.active) || pools[0]).name);
  }, [pools, selected]);

  const reload = async () => { await loadPools(); await loadVolumes(selected); };
  const pool = pools.find((p) => p.name === selected);
  const doPool = (p, action, confirm) => act.run(async () => { const r = await api(`${poolPath(p.name)}/${action}`, jsonOptions('POST', {})); await reload(); return r; }, { confirm });
  const closeModal = () => { act.setError(''); setModal(null); };

  return <>
    <PageHeader eyebrow="STOCKAGE · LIBVIRT" title="Pools, volumes et ISO" subtitle="Les pools « dir » acceptent les écritures ; les autres types sont affichés en lecture seule.">
      <button type="button" className="secondary-button" onClick={reload}>↻ Actualiser</button>
      {admin && <button type="button" className="refresh-button" onClick={() => { act.setError(''); setModal({ kind: 'pool' }); }}>+ Créer un pool</button>}
    </PageHeader>
    {loadError && <div className="error-panel" role="alert"><span>{loadError}</span></div>}
    <Banner error={!modal && act.error} notice={act.notice} onClear={(k) => (k === 'error' ? act.setError('') : act.setNotice(''))} />
    <section className="table-card"><div className="table-header"><div><h2>Pools de stockage</h2><p>Sélectionnez un pool pour voir ses volumes.</p></div></div>
      <div className="table-scroll"><table><thead><tr><th>Nom</th><th>Type</th><th>État</th><th>Occupation</th><th>Actions</th></tr></thead><tbody>
        {pools.map((p) => <tr key={p.name} className={p.name === selected ? 'row-selected' : ''}>
          <td><button type="button" className="link-button strong" onClick={() => setSelected(p.name)}>{p.name}</button><span className="uuid">{p.target_path || '—'}</span></td>
          <td>{p.pool_type}</td>
          <td><span className={`state-badge ${p.active ? 'state-running' : 'state-shutoff'}`}><span className="state-dot" />{p.active ? 'Actif' : 'Inactif'}</span>{p.autostart && <span className="managed-label">auto</span>}</td>
          <td><UsageBar pool={p} /></td>
          <td>{admin ? <div className="actions">
            {!p.active && <button type="button" className="vm-action primary" disabled={act.busy} onClick={() => doPool(p, 'start')}>Démarrer</button>}
            {p.active && <button type="button" className="vm-action neutral" disabled={act.busy} onClick={() => doPool(p, 'refresh')}>Rafraîchir</button>}
            {p.active && <button type="button" className="vm-action neutral" disabled={act.busy} onClick={() => doPool(p, 'stop', `Arrêter le pool « ${p.name} » ? Les fichiers sont conservés.`)}>Arrêter</button>}
            <button type="button" className="vm-action danger" disabled={act.busy} onClick={() => { act.setError(''); setModal({ kind: 'delete-pool', pool: p }); }}>Retirer</button>
          </div> : <span className="muted">lecture seule</span>}</td>
        </tr>)}
        {!pools.length && <tr><td className="empty" colSpan={5}>Aucun pool de stockage.</td></tr>}
      </tbody></table></div></section>

    {pool && <section className="table-card"><div className="table-header">
      <div><h2>Volumes de « {pool.name} »</h2><p>{pool.active ? `${volumes.length} volume(s)` : 'Pool inactif : démarrez-le pour lister ses volumes.'}</p></div>
      {admin && pool.active && pool.pool_type === 'dir' && <button type="button" className="refresh-button" onClick={() => { act.setError(''); setModal({ kind: 'volume' }); }}>+ Nouveau volume</button>}
    </div>
      {admin && pool.active && pool.pool_type === 'dir' && <IsoUpload pool={pool.name} existing={volumes.map((v) => v.name)} onDone={reload} />}
      <div className="table-scroll"><table><thead><tr><th>Nom</th><th>Type</th><th>Taille</th><th>Utilisé par</th>{admin && <th>Actions</th>}</tr></thead><tbody>
        {volumes.map((v) => <tr key={v.name}>
          <td><strong>{v.name}</strong><span className="uuid">{v.path}</span></td>
          <td>{v.kind === 'iso' ? 'ISO' : v.kind === 'disk' ? `Disque ${v.format}` : v.format || 'autre'}</td>
          <td>{formatBytes(v.capacity_bytes)}<span className="uuid">occupé : {formatBytes(v.allocation_bytes)}</span></td>
          <td>{v.in_use_by.length ? v.in_use_by.join(', ') : <span className="muted">libre</span>}</td>
          {admin && <td><button type="button" className="vm-action danger" disabled={act.busy || v.in_use_by.length > 0 || pool.pool_type !== 'dir'} title={v.in_use_by.length ? 'Volume utilisé par une VM' : ''} onClick={() => { act.setError(''); setModal({ kind: 'delete-volume', volume: v }); }}>Supprimer</button></td>}
        </tr>)}
        {!volumes.length && <tr><td className="empty" colSpan={admin ? 5 : 4}>Aucun volume.</td></tr>}
      </tbody></table></div></section>}

    {modal?.kind === 'pool' && <CreatePoolModal act={act} onClose={closeModal} onSubmit={async (body) => { if (await act.run(async () => { const r = await api('/api/v1/storage/pools', jsonOptions('POST', body)); await reload(); return r; })) setModal(null); }} />}
    {modal?.kind === 'volume' && <CreateVolumeModal pool={selected} act={act} onClose={closeModal} onSubmit={async (body) => { if (await act.run(async () => { const r = await api(`${poolPath(selected)}/volumes`, jsonOptions('POST', body)); await reload(); return r; })) setModal(null); }} />}
    {modal?.kind === 'delete-pool' && <ConfirmDelete title="Retirer un pool" name={modal.pool.name} act={act} onClose={closeModal}
      warning={`Le pool « ${modal.pool.name} » sera retiré de libvirt. Les fichiers du répertoire ne sont pas supprimés. Refusé si une VM utilise un de ses volumes.`}
      onSubmit={async (typed) => { if (await act.run(async () => { const r = await api(poolPath(modal.pool.name), jsonOptions('DELETE', { confirm_name: typed })); setSelected(''); await loadPools(); return r; })) setModal(null); }} />}
    {modal?.kind === 'delete-volume' && <ConfirmDelete title="Supprimer un volume" name={modal.volume.name} act={act} onClose={closeModal}
      warning={`Le volume « ${modal.volume.name} » sera supprimé DÉFINITIVEMENT (${formatBytes(modal.volume.allocation_bytes)} de données).`}
      onSubmit={async (typed) => { if (await act.run(async () => { const r = await api(`${poolPath(selected)}/volumes/${encodeURIComponent(modal.volume.name)}`, jsonOptions('DELETE', { confirm_name: typed })); await reload(); return r; })) setModal(null); }} />}
  </>;
}
