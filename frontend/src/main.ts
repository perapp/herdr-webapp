import { mount } from 'svelte';
import App from './App.svelte';
import './style.css';
import { initializeAppearance } from './lib/appearance';

initializeAppearance();

// Consume a one-time link before any network requests. Never persist the token.
const fragment = new URLSearchParams(location.hash.slice(1));
let token = fragment.get('token') || '';
if (fragment.has('token')) {
  history.replaceState(null, '', location.pathname + location.search);
}
mount(App, { target: document.getElementById('app')!, props: { initialToken: token } });
token = '';
fragment.delete('token');
