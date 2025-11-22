// src/modules/ui-mods.js (o dove hai la funzione)

import { formatBytes } from './utils.js'; // Assicurati che l'import sia corretto

export function renderModsList(mods) {
  const modsListEl = document.getElementById("mods-list");
  // Selettore aggiornato per sicurezza
  const modsHeaderEl = document.querySelector(".mods-header h3") || document.getElementById("mods-header-title"); 

  const count = mods ? mods.length : 0;
  if(modsHeaderEl) modsHeaderEl.textContent = `Mod Installate (${count})`;
  
  modsListEl.innerHTML = "";

  if (!mods || mods.length === 0) {
    // Ho aggiunto grid-column: span 2 per centrare il messaggio su tutta la larghezza
    modsListEl.innerHTML = `<li style="grid-column: span 2; padding:30px; text-align:center; color:var(--text-muted); list-style:none;">Nessuna mod installata</li>`;
    return;
  }

  mods.forEach(mod => {
    const li = document.createElement("li");
    li.className = "mod-item";
    const sizeFormatted = formatBytes(mod.size);

    // Nota: "checked" è hardcoded per ora. In futuro leggeremo se il file finisce con .disabled
    const isEnabled = true; 

    li.innerHTML = `
      <div class="mod-info">
        <span class="mod-name" title="${mod.name}">${mod.name}</span>
        <span class="mod-size">${sizeFormatted}</span>
      </div>

      <div class="mod-actions">
        <label class="switch mod-switch" title="Attiva/Disattiva">
          <input type="checkbox" ${isEnabled ? 'checked' : ''} class="mod-toggle-input" data-filename="${mod.name}">
          <span class="slider round"></span>
        </label>

        <button class="btn-delete-mod" title="Elimina Mod" data-filename="${mod.name}">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="3 6 5 6 21 6"></polyline>
            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
          </svg>
        </button>
      </div>
    `;
    
    modsListEl.appendChild(li);
  });
}