import { useState } from 'react';
import { api, jsonOptions } from '../api.js';

export default function Login({ onLogin }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  async function submit(e) {
    e.preventDefault();
    setBusy(true); setError('');
    try {
      onLogin(await api('/api/v1/auth/login', jsonOptions('POST', { username, password })));
    } catch (err) { setError(err.message); setPassword(''); } finally { setBusy(false); }
  }
  return <div className="login-screen">
    <form className="login-card" onSubmit={submit}>
      <div className="brand login-brand"><span className="brand-mark">V</span><span>Virt<span className="brand-light">Manager</span></span></div>
      <h1>Connexion</h1>
      <p className="subtitle">Accès réservé aux comptes autorisés.</p>
      {error && <div className="error-panel" role="alert"><span>{error}</span></div>}
      <label className="field">Identifiant<input autoFocus required autoComplete="username" value={username} onChange={(e) => setUsername(e.target.value)} /></label>
      <label className="field">Mot de passe<input required type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} /></label>
      <button type="submit" className="refresh-button login-submit" disabled={busy || !username || !password}>{busy ? 'Connexion…' : 'Se connecter'}</button>
      <p className="form-tip">Premier lancement : l'identifiant est <strong>admin</strong> et le mot de passe est affiché dans le terminal du serveur (et dans <code>data/admin-initial-password.txt</code>).</p>
    </form>
  </div>;
}
