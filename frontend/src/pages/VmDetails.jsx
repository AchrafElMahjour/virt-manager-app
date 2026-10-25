import { useCallback, useEffect, useRef, useState } from 'react';
import { api, downloadText, getHv, hasRole, jsonOptions as body, vmPath } from '../api.js';
import { stateLabels } from '../common.jsx';

const number = new Intl.NumberFormat('fr-FR');

function VncConsole({ vm }) {
  const container = useRef(null);
  const [status, setStatus] = useState('Chargement de la console…');
  const [password, setPassword] = useState('');
  const rfb = useRef(null);
  useEffect(() => {
    let cancelled = false;
    async function connect() {
      try {
        const consoleInfo = await api(`${vmPath(vm.uuid)}/console`);
        if (cancelled) return;
        if (!consoleInfo.available || !consoleInfo.websocket_path) { setStatus(consoleInfo.message); return; }
        const { default: RFB } = await import('@novnc/novnc');
        if (cancelled || !container.current) return;
        const protocol = location.protocol === 'https:' ? 'wss' : 'ws';
        const url = `${protocol}://${location.host}${consoleInfo.websocket_path}?hv=${encodeURIComponent(getHv())}`;
        const client = new RFB(container.current, url, { shared: false });
        rfb.current = client;
        client.scaleViewport = true;
        client.resizeSession = false;
        client.addEventListener('connect', () => setStatus('Console VNC connectée'));
        client.addEventListener('disconnect', () => setStatus('Console déconnectée'));
        /*
        client.addEventListener('disconnect', (event) => {
          console.error('noVNC disconnect:', event.detail);

          setStatus(
            event.detail?.clean
              ? 'Console fermée normalement'
              : 'Console déconnectée : erreur de connexion VNC'
          );
        });
        */
        client.addEventListener('securityfailure', () => setStatus('Erreur d’authentification VNC'));
        client.addEventListener('credentialsrequired', () => setStatus('Mot de passe VNC demandé'));
      } catch (error) { if (!cancelled) setStatus(error.message); }
    }
    connect();
    return () => { cancelled = true; try { rfb.current?.disconnect(); } catch {} rfb.current = null; };
  }, [vm.uuid]);
  return <div className="console-content">
    <p className="form-tip">Console VNC : connexion WebSocket relayée par Rust vers un port VNC local. Les VM SPICE restent accessibles avec virt-manager / remote-viewer.</p>
    <div className="vnc-status">{status}</div>
    {status.includes('Mot de passe') && <form className="console-password" onSubmit={(e) => { e.preventDefault(); rfb.current?.sendCredentials({ password }); setPassword(''); }}>
      <input type="password" aria-label="Mot de passe VNC" value={password} onChange={(e) => setPassword(e.target.value)} placeholder="Mot de passe VNC" />
      <button type="submit" className="secondary-button">Envoyer</button>
    </form>}
    <div className="vnc-screen" ref={container} />
  </div>;
}

function IacPanel({ vm }) {
  const [tool, setTool] = useState('terraform');
  const [file, setFile] = useState(null);
  const [error, setError] = useState('');
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    let cancelled = false;
    setFile(null); setError(''); setCopied(false);
    api(`${vmPath(vm.uuid)}/export/${tool}`).then((f) => { if (!cancelled) setFile(f); }).catch((e) => { if (!cancelled) setError(e.message); });
    return () => { cancelled = true; };
  }, [vm.uuid, tool]);
  async function copy() {
    try { await navigator.clipboard.writeText(file.content); setCopied(true); } catch { setError('Copie impossible : sélectionnez le texte manuellement.'); }
  }
  return <div className="iac-panel">
    <div className="iac-tools">{['terraform', 'ansible', 'packer'].map((t) => <button type="button" key={t} className={`vm-action ${tool === t ? 'primary' : 'neutral'}`} onClick={() => setTool(t)}>{t[0].toUpperCase() + t.slice(1)}</button>)}</div>
    <p className="form-tip">Génère une VM <strong>équivalente</strong> (vCPU, RAM, disques, réseau) avec des disques vierges : aucune donnée ni aucun secret n'est exporté. Les fichiers sont fournis sans garantie de validation par l'outil cible : relisez-les avant usage.</p>
    {error && <div className="error-panel" role="alert">{error}</div>}
    {file && <>
      <div className="iac-actions"><code>{file.filename}</code>
        <button type="button" className="vm-action neutral" onClick={copy}>{copied ? 'Copié ✓' : 'Copier'}</button>
        <button type="button" className="vm-action neutral" onClick={() => downloadText(file.filename, file.content)}>Télécharger</button></div>
      <pre className="code-block" tabIndex={0}>{file.content}</pre>
    </>}
  </div>;
}

function MediaControl({ disk, vmUuid, resources, enabled, apply, pending }) {
  const [choice, setChoice] = useState('');
  return <div className="disk-controls">
    <label>Changer d'ISO<select value={choice} onChange={(e) => setChoice(e.target.value)}>
      <option value="">— choisir une ISO —</option>
      {resources.isos.map((iso) => <option key={`${iso.pool}/${iso.name}`} value={`${iso.pool}/${iso.name}`}>{iso.pool} / {iso.name}</option>)}
    </select></label>
    <button type="button" className="vm-action neutral" disabled={!enabled || pending || !choice} onClick={() => { const [pool, ...rest] = choice.split('/'); apply(`${vmPath(vmUuid)}/media`, 'PUT', { iso_pool: pool, iso_name: rest.join('/') }); }}>Insérer</button>
    {disk.source && <button type="button" className="vm-action danger" disabled={!enabled || pending} onClick={() => apply(`${vmPath(vmUuid)}/media`, 'PUT', { iso_pool: null, iso_name: null }, 'Éjecter l\'ISO du lecteur ?')}>Éjecter</button>}
  </div>;
}

export default function VmDetails({ vm, user, resources, onClose, onModified }) {
  const admin = hasRole(user, 'admin');
  const operator = hasRole(user, 'operator');
  const [detail, setDetail] = useState(null);
  const [tab, setTab] = useState('overview');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [pending, setPending] = useState(false);
  const [hardware, setHardware] = useState({ vcpus: vm.vcpus, memory_mib: vm.memory_mib });
  const [diskPool, setDiskPool] = useState(resources.pools.find((p) => p.name === 'default')?.name || resources.pools[0]?.name || '');
  const [diskSize, setDiskSize] = useState(10);
  const [grow, setGrow] = useState({});

  const refresh = useCallback(async () => {
    const value = await api(vmPath(vm.uuid));
    setDetail(value);
    setHardware({ vcpus: value.vcpus, memory_mib: value.memory_mib });
  }, [vm.uuid]);
  useEffect(() => { refresh().catch((e) => setError(e.message)); }, [refresh]);
  const offline = detail?.state === 'shutoff';

  const apply = async (url, method, params, confirmation) => {
    if (pending) return;
    if (confirmation && !window.confirm(confirmation)) return;
    setPending(true); setError(''); setNotice('');
    try {
      const result = await api(url, body(method, params));
      setNotice(result.message);
      await refresh();
      await onModified();
    } catch (e) { setError(e.message); }
    finally { setPending(false); }
  };
  return <div className="modal-scrim" role="presentation">
    <div className="modal-panel detail-modal" role="dialog" aria-modal="true" aria-label={`Détails ${vm.name}`}>
      <div className="modal-top">
        <div><h2>{vm.name} <span className="detail-secondary">{stateLabels[detail?.state || vm.state] || detail?.state || vm.state}</span></h2>
          <div className="detail-id">{vm.uuid}</div></div>
        <button type="button" className="close-button" aria-label="Fermer" onClick={() => !pending && onClose()}>×</button>
      </div>
      <nav className="detail-tabs" aria-label="Sections de la VM">
        {[['overview','Vue générale'], ['hardware','CPU / RAM'], ['disks','Stockage'], ...(operator ? [['console','Console VNC']] : []), ['iac','Modèles IaC']].map(([key,label]) =>
          <button type="button" key={key} className={tab === key ? 'selected' : ''} onClick={() => {setTab(key);setError('');}}>{label}</button>)}
      </nav>
      <div className="detail-body">
        {error && <div role="alert" className="error-panel">{error}</div>}
        {notice && <div role="status" className="notice-panel">✓ {notice}</div>}
        {!detail ? <p>Chargement des informations libvirt…</p> : <>
          {tab === 'overview' && <>
            <div className="detail-grid">
              <div><span>Statut</span><strong>{stateLabels[detail.state] || detail.state}</strong></div>
              <div><span>vCPU</span><strong>{detail.vcpus}</strong></div>
              <div><span>Mémoire allouée</span><strong>{number.format(detail.memory_mib)} Mio</strong></div>
              <div><span>Console</span><strong>{detail.graphics || 'Aucune'}</strong></div>
              <div><span>Gestion</span><strong>{detail.managed ? 'Application' : 'Externe'}</strong></div>
              <div><span>Réseau(x)</span><strong>{detail.networks.join(', ') || 'Isolée'}</strong></div>
            </div>
            <p className="form-tip">Les informations proviennent du XML libvirt et non d'un inventaire statique. Les disques externes sont visibles, mais leur suppression depuis cette application est interdite.</p>
          </>}
          {tab === 'hardware' && <form className="detail-form" onSubmit={(e) => {e.preventDefault();apply(`${vmPath(vm.uuid)}/hardware`,'PUT', {vcpus:Number(hardware.vcpus),memory_mib:Number(hardware.memory_mib)});}}>
            <div className="field-grid">
              <label className="field">Nombre de vCPU<input type="number" required min="1" max="32" value={hardware.vcpus} onChange={(e) => setHardware((p) => ({...p,vcpus:e.target.value}))}/></label>
              <label className="field">RAM (Mio)<input type="number" required min="256" max="131072" value={hardware.memory_mib} onChange={(e) => setHardware((p) => ({...p,memory_mib:e.target.value}))}/></label>
            </div>
            <p className="form-tip">Configuration persistante à froid uniquement. Les topologies CPU/NUMA avancées ne sont pas modifiées. Aucun changement à chaud dans cette version.</p>
            <div className="modal-footer"><button type="submit" className="refresh-button" disabled={!offline || !admin || pending}>{pending ? 'Mise à jour…' : 'Enregistrer CPU/RAM'}</button></div>
          </form>}
          {tab === 'disks' && <>
            <div className="disk-list">{detail.disks.map((disk) => <div className="disk-entry" key={disk.target}>
              <div className="disk-entry-main"><strong>{disk.target} · {disk.device}</strong> <span>{disk.format || '—'} · {disk.capacity_gib != null ? `${disk.capacity_gib} Gio` : 'Capacité indisponible'}</span>
                <small title={disk.source || ''}>{disk.source || 'Aucun média'}</small></div>
              {detail.managed && disk.app_managed && disk.device === 'disk' && <div className="disk-controls">
                <label>Taille cible (Gio)<input type="number" min={(disk.capacity_gib || 0)+1} max="2048" value={grow[disk.target] ?? ((disk.capacity_gib || 0) + 1)} onChange={(e) => setGrow((p) => ({...p,[disk.target]:e.target.value}))}/></label>
                <button type="button" className="vm-action neutral" disabled={!offline || !admin || pending} onClick={() => apply(`${vmPath(vm.uuid)}/disks/${disk.target}/resize`,'POST',{disk_gib:Number(grow[disk.target] ?? ((disk.capacity_gib || 0)+1))},`Agrandir définitivement le disque ${disk.target} ?`)}>Agrandir</button>
                {disk.target !== 'vda' && <button type="button" className="vm-action danger" disabled={!offline || !admin || pending} onClick={() => apply(`${vmPath(vm.uuid)}/disks/${disk.target}`,'DELETE',{delete_disk:true},`Détacher et SUPPRIMER définitivement le volume ${disk.target} ? Ses données seront perdues.`)}>Détacher + supprimer</button>}
              </div>}
              {admin && disk.device === 'cdrom' && <MediaControl disk={disk} vmUuid={vm.uuid} resources={resources} enabled={offline} apply={apply} pending={pending} />}
            </div>)}</div>
            <form className="detail-form" onSubmit={(e)=>{e.preventDefault();apply(`${vmPath(vm.uuid)}/disks`,'POST',{pool:diskPool,disk_gib:Number(diskSize)});}}>
              <h3>Ajouter un disque QCOW2</h3>
              <div className="field-grid">
                <label className="field">Pool<select required value={diskPool} onChange={(e) => setDiskPool(e.target.value)}>{resources.pools.map((pool)=><option key={pool.name} value={pool.name}>{pool.name} · {pool.free_gib} Gio libres</option>)}</select></label>
                <label className="field">Taille (Gio)<input type="number" required min="1" max="2048" value={diskSize} onChange={(e)=>setDiskSize(e.target.value)}/></label>
              </div>
              <div className="modal-footer"><button className="refresh-button" type="submit" disabled={!offline || !admin || !detail.managed || !diskPool || pending}>Ajouter le disque</button></div>
            </form>
            <p className="form-tip">Ajout, détachement et agrandissement à froid, pour les VM gérées uniquement. Agrandir un volume ne redimensionne pas automatiquement la partition ni le système de fichiers du système invité. Le disque principal n'est jamais détachable ici.</p>
          </>}
          {tab === 'iac' && <IacPanel vm={vm} />}
          {tab === 'console' && operator && <>{detail.state==='running' ? <VncConsole vm={detail}/> : <p className="form-tip">Démarrez cette VM avant d'ouvrir la console. Les machines en pause ne sont pas accessibles via cette interface.</p>}</>}
        </>}
      </div>
      <div className="detail-bottom"><button type="button" className="secondary-button" onClick={onClose} disabled={pending}>Fermer</button></div>
    </div>
  </div>;
}
