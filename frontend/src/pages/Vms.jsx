import { useCallback, useEffect, useMemo, useState } from 'react';
import VmDetails from './VmDetails.jsx';
import { api, getHv, hasRole, jsonOptions } from '../api.js';
import { Banner, Metric, Modal, PageHeader, StateBadge } from '../common.jsx';

const formatter = new Intl.NumberFormat('fr-FR');

function VmActions({ vm, busy, user, onAction, onClone, onDelete, onDetail }) {
  const operator = hasRole(user, 'operator');
  const admin = hasRole(user, 'admin');
  let actions = [];
  if (operator && vm.state === 'shutoff') actions = [{ key: 'start', label: '▶ Démarrer', cls: 'primary' }];
  else if (operator && vm.state === 'paused') actions = [{ key: 'resume', label: '▶ Reprendre', cls: 'primary' }];
  else if (operator && vm.state === 'running') actions = [
    { key: 'suspend', label: 'Ⅱ Pause', cls: 'neutral' },
    { key: 'shutdown', label: '⏻ Éteindre', cls: 'danger' },
  ];
  return <div className="actions">
    <button type="button" className="vm-action neutral" disabled={Boolean(busy)} onClick={() => onDetail(vm)}>Détails</button>
    {actions.map((action) => <button type="button" key={action.key} className={`vm-action ${action.cls}`}
      onClick={() => onAction(vm, action.key)} disabled={Boolean(busy)}>{action.label}</button>)}
    {admin && vm.state === 'shutoff' && <button type="button" className="vm-action neutral" disabled={Boolean(busy)} onClick={() => onClone(vm)}>⧉ Cloner</button>}
    {admin && vm.state === 'shutoff' && vm.managed && <button type="button" className="vm-action danger" disabled={Boolean(busy)} onClick={() => onDelete(vm)}>Supprimer</button>}
    {busy === vm.uuid && <span className="pending">Traitement…</span>}
  </div>;
}

const initialCreate = { name: '', vcpus: 2, memory_mib: 2048, disk_gib: 20, pool: '', network: '', iso: '' };
function CreateModal({ resources, busy, error, onClose, onSubmit }) {
  const [form, setForm] = useState(() => ({
    ...initialCreate, pool: resources.pools.find((item) => item.name === 'default')?.name ?? resources.pools[0]?.name ?? '',
    network: resources.networks.includes('default') ? 'default' : (resources.networks[0] ?? ''),
  }));
  const setField = (key, value) => setForm((f) => ({ ...f, [key]: value }));
  const submit = (e) => {
    e.preventDefault();
    const chosenIso = resources.isos.find((iso) => `${iso.pool}/${iso.name}` === form.iso);
    onSubmit({
      name: form.name.trim(), vcpus: Number(form.vcpus), memory_mib: Number(form.memory_mib),
      disk_gib: Number(form.disk_gib), pool: form.pool,
      network: form.network || null, iso_pool: chosenIso?.pool ?? null, iso_name: chosenIso?.name ?? null,
    });
  };
  return <Modal title="Créer une machine virtuelle" onClose={onClose}>
    <form onSubmit={submit} className="modal-body">
      {error && <div className="error-panel" role="alert">{error}</div>}
      <p className="modal-intro">La VM sera définie <strong>à l'arrêt</strong>, avec un nouveau disque QCOW2 géré par libvirt.</p>
      <label className="field">Nom de la VM<input required maxLength={63} pattern="[A-Za-z0-9][A-Za-z0-9_-]*" placeholder="debian-lab-01" value={form.name} onChange={(e) => setField('name', e.target.value)} /></label>
      <div className="field-grid">
        <label className="field">vCPU<input required type="number" min="1" max="32" value={form.vcpus} onChange={(e) => setField('vcpus', e.target.value)} /></label>
        <label className="field">RAM (Mio)<input required type="number" min="256" max="131072" step="256" value={form.memory_mib} onChange={(e) => setField('memory_mib', e.target.value)} /></label>
      </div>
      <div className="field-grid">
        <label className="field">Disque QCOW2 (Gio)<input required type="number" min="1" max="2048" value={form.disk_gib} onChange={(e) => setField('disk_gib', e.target.value)} /></label>
        <label className="field">Pool de stockage<select required value={form.pool} onChange={(e) => setField('pool', e.target.value)}>
          {resources.pools.length === 0 && <option value="">Aucun pool disponible</option>}
          {resources.pools.map((pool) => <option value={pool.name} key={pool.name}>{pool.name} · libre ≈ {pool.free_gib} Gio</option>)}
        </select></label>
      </div>
      <label className="field">Réseau libvirt<select value={form.network} onChange={(e) => setField('network', e.target.value)}>
        <option value="">Aucun réseau (VM isolée)</option>
        {resources.networks.map((name) => <option value={name} key={name}>{name}</option>)}
      </select></label>
      <label className="field">Image ISO d'installation (facultatif)<select value={form.iso} onChange={(e) => setField('iso', e.target.value)}>
        <option value="">Sans ISO — disque vierge</option>
        {resources.isos.map((iso) => <option key={`${iso.pool}/${iso.name}`} value={`${iso.pool}/${iso.name}`}>{iso.pool} / {iso.name}</option>)}
      </select></label>
      <p className="form-tip">Sans ISO, la VM est créée mais n'a aucun système d'exploitation. Les ISO doivent se trouver dans un pool libvirt de type « dir » actif.</p>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={busy || resources.pools.length === 0}>{busy ? 'Création…' : 'Créer la VM'}</button></div>
    </form>
  </Modal>;
}
function CloneModal({ vm, busy, error, onClose, onSubmit }) {
  const [name, setName] = useState(`${vm.name}-clone2`);
  return <Modal title={`Cloner « ${vm.name} »`} onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit({ name: name.trim() }); }}>
      {error && <div className="error-panel" role="alert">{error}</div>}
      <p className="modal-intro">Copie complète du disque via libvirt. Le clone restera arrêté. La VM source doit être totalement arrêtée.</p>
      <label className="field">Nom du clone<input required maxLength={63} pattern="[A-Za-z0-9][A-Za-z0-9_-]*" value={name} onChange={(e) => setName(e.target.value)} /></label>
      <p className="form-tip">Version limitée aux VM BIOS avec un seul disque fichier dans un pool actif. Le clone reçoit un UUID et des adresses MAC distincts ; son hostname et son machine-id internes à l'OS restent à adapter.</p>
      <div className="modal-footer"><button className="secondary-button" type="button" onClick={onClose}>Annuler</button><button type="submit" className="refresh-button" disabled={busy}>{busy ? 'Copie du disque…' : 'Cloner à froid'}</button></div>
    </form>
  </Modal>;
}
function DeleteModal({ vm, busy, error, onClose, onSubmit }) {
  const [confirmName, setConfirmName] = useState('');
  const [deleteDisk, setDeleteDisk] = useState(false);
  return <Modal title="Supprimer une VM" onClose={onClose}>
    <form className="modal-body" onSubmit={(e) => { e.preventDefault(); onSubmit({ confirm_name: confirmName, delete_disk: deleteDisk }); }}>
      {error && <div className="error-panel" role="alert">{error}</div>}
      <p className="delete-warning">Vous allez retirer la définition libvirt de <strong>{vm.name}</strong>. Cette action ne peut pas être annulée depuis l'application.</p>
      <label className="field">Tapez exactement <strong>{vm.name}</strong> pour confirmer<input required autoComplete="off" value={confirmName} onChange={(e) => setConfirmName(e.target.value)} /></label>
      <label className="checkbox-row"><input type="checkbox" checked={deleteDisk} onChange={(e) => setDeleteDisk(e.target.checked)} /> Supprimer aussi le volume QCOW2 géré (perte définitive des données)</label>
      <p className="form-tip">Si cette case reste décochée, seul le domaine sera supprimé et son disque sera conservé dans le pool. La suppression n'est autorisée que pour les VM créées ou clonées avec cette application.</p>
      <div className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>Annuler</button><button className="delete-button" type="submit" disabled={busy || confirmName !== vm.name}>{busy ? 'Suppression…' : 'Confirmer la suppression'}</button></div>
    </form>
  </Modal>;
}


export default function Vms({ user }) {
  const admin = hasRole(user, 'admin');
  const [vms, setVms] = useState([]);
  const [resources, setResources] = useState({ pools: [], networks: [], isos: [] });
  const [resourcesError, setResourcesError] = useState('');
  const [connectionError, setConnectionError] = useState('');
  const [actionError, setActionError] = useState('');
  const [notice, setNotice] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(null);
  const [search, setSearch] = useState('');
  const [lastUpdated, setLastUpdated] = useState(null);
  const [modal, setModal] = useState(null);
  const [hypervisor, setHypervisor] = useState(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api('/api/v1/vms');
      setVms(list);
      setConnectionError('');
      setLastUpdated(new Date());
    } catch (error) {
      setConnectionError(error.message || 'Impossible de joindre le serveur Rust');
    } finally { setLoading(false); }
    try {
      setResources(await api('/api/v1/resources'));
      setResourcesError('');
    } catch (error) { setResourcesError(error.message); }
  }, []);

  useEffect(() => {
    api('/api/v1/hypervisors').then(async (list) => {
      const hv = getHv();
      const entry = list.find((h) => h.id === hv);
      try {
        const info = await api(`/api/v1/hypervisors/${encodeURIComponent(hv)}/info`);
        setHypervisor({ ...info, name: entry?.name });
      } catch { setHypervisor(null); }
    }).catch(() => setHypervisor(null));
    refresh();
    const timer = setInterval(refresh, 10000);
    return () => clearInterval(timer);
  }, [refresh]);

  async function runAction(vm, action) {
    if (busy) return;
    if (action === 'shutdown' && !window.confirm(`Demander un arrêt propre de « ${vm.name} » ?\nCette opération peut prendre du temps.`)) return;
    setBusy(vm.uuid); setActionError(''); setNotice('');
    try {
      const result = await api(`/api/v1/vms/${encodeURIComponent(vm.uuid)}/${action}`, jsonOptions('POST', {}));
      setNotice(`${result.name} : ${result.message}`);
      await refresh();
    } catch (error) { setActionError(`${vm.name} : ${error.message}`); }
    finally { setBusy(null); }
  }
  async function submitProvision(url, method, body) {
    if (busy) return;
    setBusy(modal?.vm?.uuid || 'provision'); setActionError(''); setNotice('');
    try {
      const result = await api(url, jsonOptions(method, body));
      setModal(null);
      setNotice(`${result.name} : ${result.message}`);
      await refresh();
    } catch (error) { setActionError(error.message); }
    finally { setBusy(null); }
  }
  const filteredVms = useMemo(() => vms.filter((vm) => `${vm.name} ${vm.uuid} ${vm.state}`.toLowerCase().includes(search.toLowerCase().trim())), [vms, search]);
  const running = vms.filter((vm) => vm.state === 'running').length;
  const paused = vms.filter((vm) => vm.state === 'paused').length;
  const stopped = vms.filter((vm) => vm.state === 'shutoff').length;
  const allocatedRam = vms.reduce((sum, vm) => sum + vm.memory_mib, 0);
  const open = (kind) => (vm) => { setActionError(''); setModal({ kind, vm }); };
  return <>
    <PageHeader eyebrow="VIRTUALISATION · KVM / QEMU" title="Machines virtuelles" subtitle="Gestion des VM, CPU/RAM, volumes QCOW2 et console VNC via libvirt.">
      <button type="button" className="secondary-button" onClick={refresh} disabled={loading}>↻ Actualiser</button>
      {admin && <button type="button" className="refresh-button" onClick={() => { setActionError(''); setModal({ kind: 'create' }); }} disabled={Boolean(busy) || !!connectionError || !!resourcesError || resources.pools.length === 0}>+ Créer une VM</button>}
    </PageHeader>
    <div className="host-card"><span className={`host-status ${connectionError ? 'host-offline' : ''}`} /><div className="host-information">
      <strong>{hypervisor?.hostname || hypervisor?.name || 'Hyperviseur'}</strong>
      <span>{hypervisor ? `${hypervisor.hypervisor_type} · ${hypervisor.uri}` : 'Connexion libvirt'}</span>
    </div><span className="host-pill">{connectionError ? 'Indisponible' : lastUpdated ? 'Connecté' : 'Connexion…'}</span></div>
    {connectionError && <div className="error-panel" role="alert"><span><strong>Erreur de connexion :</strong> {connectionError}</span></div>}
    {resourcesError && !connectionError && <div className="error-panel" role="alert"><span><strong>Ressources indisponibles :</strong> {resourcesError}</span></div>}
    {!resourcesError && !resources.pools.length && !connectionError && <div className="info-panel">Aucun pool de stockage « dir » actif. Activez un pool dans « Stockage & ISO » pour pouvoir créer des VM.</div>}
    <Banner error={actionError && `Action refusée : ${actionError}`} notice={notice} onClear={(k) => (k === 'error' ? setActionError('') : setNotice(''))} />
    <section className="metrics">
      <Metric label="Nombre de VM" value={formatter.format(vms.length)} hint="Inventaire libvirt" />
      <Metric label="VM en cours" value={formatter.format(running)} hint={`VM arrêtées : ${stopped}`} />
      <Metric label="VM en pause" value={formatter.format(paused)} hint="Reprenez-les sans redémarrer" />
      <Metric label="RAM attribuée" value={`${formatter.format(allocatedRam)} Mio`} hint="Total configuré (non physique)" />
    </section>
    <section className="table-card"><div className="table-header">
      <div><h2>Inventaire des VM</h2><p>{lastUpdated ? `Dernière actualisation : ${lastUpdated.toLocaleTimeString('fr-FR')}` : 'Synchronisation en cours'}</p></div>
      <input aria-label="Rechercher une VM" placeholder="Rechercher une VM…" value={search} onChange={(e) => setSearch(e.target.value)} />
    </div><div className="table-scroll"><table><thead><tr><th>Nom</th><th>État</th><th>vCPU</th><th>Mémoire</th><th>Identifiant</th><th>Actions</th></tr></thead>
      <tbody>{filteredVms.map((vm) => <tr key={vm.uuid}>
        <td><strong>{vm.name}{vm.managed && <span className="managed-label">Gérée</span>}</strong><span className="uuid">{vm.uuid}</span></td>
        <td><StateBadge state={vm.state} /></td><td>{vm.vcpus}</td><td>{formatter.format(vm.memory_mib)} Mio</td><td>{vm.id ?? '—'}</td>
        <td><VmActions vm={vm} user={user} busy={busy} onAction={runAction} onClone={open('clone')} onDelete={open('delete')} onDetail={open('details')} /></td>
      </tr>)}
      {!filteredVms.length && <tr><td className="empty" colSpan={6}>{loading ? 'Chargement des VM…' : search ? 'Aucune VM ne correspond à la recherche.' : 'Aucune machine virtuelle trouvée.'}</td></tr>}
      </tbody></table></div></section>
    <div className="resource-summary">Pools disponibles : {resources.pools.map((p) => p.name).join(', ') || 'aucun'} · Réseaux actifs : {resources.networks.join(', ') || 'aucun'} · Images ISO : {resources.isos.length}</div>
    <p className="read-only-note">Rôle « {user.role} » · Les clones et suppressions nécessitent une VM arrêtée · Aucun arrêt forcé.</p>
    {modal?.kind === 'details' && <VmDetails vm={modal.vm} user={user} resources={resources} onClose={() => setModal(null)} onModified={refresh} />}
    {modal?.kind === 'create' && <CreateModal resources={resources} busy={busy} error={actionError} onClose={() => !busy && setModal(null)} onSubmit={(body) => submitProvision('/api/v1/vms', 'POST', body)} />}
    {modal?.kind === 'clone' && <CloneModal vm={modal.vm} busy={busy} error={actionError} onClose={() => !busy && setModal(null)} onSubmit={(body) => submitProvision(`/api/v1/vms/${encodeURIComponent(modal.vm.uuid)}/clone`, 'POST', body)} />}
    {modal?.kind === 'delete' && <DeleteModal vm={modal.vm} busy={busy} error={actionError} onClose={() => !busy && setModal(null)} onSubmit={(body) => submitProvision(`/api/v1/vms/${encodeURIComponent(modal.vm.uuid)}`, 'DELETE', body)} />}
  </>;
}
