import { useEffect, useState } from 'react';
import { api, downloadText } from '../api.js';
import { PageHeader } from '../common.jsx';

const toolLabels = { terraform: 'Terraform', ansible: 'Ansible', packer: 'Packer' };

export default function Templates() {
  const [list, setList] = useState([]);
  const [error, setError] = useState('');
  const [open, setOpen] = useState(null);
  useEffect(() => { api('/api/v1/templates').then(setList).catch((e) => setError(e.message)); }, []);
  async function show(id) {
    setError('');
    try { setOpen(await api(`/api/v1/templates/${encodeURIComponent(id)}`)); } catch (e) { setError(e.message); }
  }
  const tools = ['terraform', 'ansible', 'packer'];
  return <>
    <PageHeader eyebrow="INFRASTRUCTURE AS CODE" title="Modèles Terraform, Ansible et Packer" subtitle="Fichiers de départ prêts à adapter. Pour exporter une VM existante, ouvrez ses détails puis l'onglet « Modèles IaC »." />
    {error && <div className="error-panel" role="alert"><span>{error}</span></div>}
    <div className="template-grid">
      {tools.map((tool) => <section className="table-card" key={tool}>
        <div className="table-header"><div><h2>{toolLabels[tool]}</h2></div></div>
        <ul className="template-list">{list.filter((t) => t.tool === tool).map((t) => <li key={t.id}>
          <div><strong>{t.title}</strong><span className="muted">{t.description}</span><code>{t.filename}</code></div>
          <div className="actions"><button type="button" className="vm-action neutral" onClick={() => show(t.id)}>Voir</button></div>
        </li>)}</ul>
      </section>)}
    </div>
    {open && <section className="table-card"><div className="table-header"><div><h2>{open.filename}</h2><p>{open.tool}</p></div>
      <div className="actions"><button type="button" className="secondary-button" onClick={() => downloadText(open.filename, open.content)}>Télécharger</button><button type="button" className="secondary-button" onClick={() => setOpen(null)}>Fermer</button></div></div>
      <pre className="code-block" tabIndex={0}>{open.content}</pre></section>}
    <p className="read-only-note">Les modèles Terraform visent le fournisseur dmacvicar/libvirt 0.8.x ; Packer attend un hôte avec /dev/kvm. Ils n'ont pas été exécutés par les outils cibles lors du développement : relisez-les avant tout usage en production.</p>
  </>;
}
