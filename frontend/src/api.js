// Couche d'accès à l'API : cookie de session (HttpOnly, géré par le navigateur) et hyperviseur courant.
let currentHv = 'local';
try { currentHv = localStorage.getItem('virtmgr.hv') || 'local'; } catch { /* stockage indisponible */ }
let unauthorizedHandler = null;

export const getHv = () => currentHv;
export function setHv(id) {
  currentHv = id;
  try { localStorage.setItem('virtmgr.hv', id); } catch { /* ignoré */ }
}
export const onUnauthorized = (fn) => { unauthorizedHandler = fn; };

export class ApiError extends Error {
  constructor(message, status) { super(message); this.status = status; }
}

export async function api(url, options = {}) {
  const { hv, ...rest } = options;
  const headers = { Accept: 'application/json', 'X-Hypervisor': hv ?? currentHv, ...rest.headers };
  const response = await fetch(url, { ...rest, headers });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    const sessionCheck = url.includes('/auth/login') || url.endsWith('/auth/me');
    if (response.status === 401 && !sessionCheck && unauthorizedHandler) unauthorizedHandler();
    throw new ApiError(payload?.error || `Erreur HTTP ${response.status}`, response.status);
  }
  return payload;
}

export const jsonOptions = (method, body) => ({
  method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
});
export const vmPath = (uuid) => `/api/v1/vms/${encodeURIComponent(uuid)}`;
export const poolPath = (name) => `/api/v1/storage/pools/${encodeURIComponent(name)}`;

// Téléversement d'ISO avec progression (XMLHttpRequest : fetch n'expose pas la progression d'envoi).
export function uploadIso(pool, name, file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('PUT', `${poolPath(pool)}/isos/${encodeURIComponent(name)}`);
    xhr.setRequestHeader('X-Hypervisor', currentHv);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.upload.onprogress = (e) => { if (e.lengthComputable) onProgress(e.loaded / e.total); };
    xhr.onload = () => {
      let payload = null;
      try { payload = JSON.parse(xhr.responseText); } catch { /* corps non JSON */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(payload);
      else if (payload?.error) reject(new Error(payload.error));
      else if (xhr.status === 0 || xhr.status === 502 || xhr.status === 504) reject(new Error(`Envoi interrompu (HTTP ${xhr.status}) : le serveur ou le reverse-proxy a coupé la connexion (nom déjà utilisé, pool plein, taille limitée ?).`));
      else reject(new Error(`Erreur HTTP ${xhr.status}`));
    };
    xhr.onerror = () => reject(new Error('Connexion interrompue pendant le téléversement'));
    xhr.send(file);
  });
}

export function downloadText(filename, content) {
  const url = URL.createObjectURL(new Blob([content], { type: 'text/plain;charset=utf-8' }));
  const a = document.createElement('a');
  a.href = url; a.download = filename;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export const rank = { viewer: 1, operator: 2, admin: 3 };
export const hasRole = (user, min) => (rank[user?.role] || 0) >= rank[min];

const nf = new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 1 });
export function formatBytes(n) {
  if (n == null) return '—';
  const units = ['o', 'Kio', 'Mio', 'Gio', 'Tio'];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return `${nf.format(v)} ${units[i]}`;
}
