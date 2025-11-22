const { invoke } = window.__TAURI__.core;
import { getServers } from './modules/api.js';
import { setupTabs } from './modules/ui-tabs.js';
import { populatePropertiesPanel } from './modules/ui-properties.js';
import { renderModsList } from './modules/ui-mods.js';
import { setupModals } from './modules/ui-modals.js';
import { setupConsole, clearConsole } from './modules/ui-console.js';

// --- GLOBAL STATE ---
// We pass this object to modules so they can read/write shared state
const state = {
  serverList: [],
  activeServerId: null
};

const runningServers = new Set();

// --- DOM REF ---
const serverListEl = document.getElementById("server-list");
const loadingEl = document.getElementById("sidebar-loading");
const emptyStateEl = document.getElementById("empty-state");
const serverDetailsEl = document.getElementById("server-details");
const detailNameEl = document.getElementById("detail-name");
const detailVersionEl = document.getElementById("detail-version");
const detailStatusEl = document.getElementById("detail-status");
const detailIconImg = document.getElementById("detail-icon");

// --- MAIN LOGIC ---

function renderSidebar() {
  serverListEl.innerHTML = "";
  
  if (state.serverList.length === 0) {
     serverListEl.innerHTML = "<p style='text-align:center; color:gray; margin-top:20px;'>Nessun server trovato.</p>";
     return;
  }

  state.serverList.forEach(server => {
    const li = document.createElement("li");
    li.className = "server-item";
    const statusClass = server.status === 'online' ? 'online' : '';
    
    li.innerHTML = `
      <div class="server-info">
        <img src="./assets/${server.icon}" alt="${server.name} Icon" class="server-icon" />
        <div class="server-text">
          <h4>${server.name}</h4>
          <p>${server.version} • ${server.last_played}</p>
        </div>
      </div>
      <div class="status-dot ${statusClass}" id="status-dot-${server.id.toLowerCase().replace(/\s+/g, '-')}"></div>
    `;
    
    li.addEventListener("click", () => selectServer(server));
    serverListEl.appendChild(li);
  });
}

async function handleServerToggle(serverId, serverStatus, btn) {
    const isRunning = runningServers.has(serverId);

    if (!isRunning) {
        // --- START LOGIC ---
        const originalText = btn.textContent;
        btn.textContent = "Avvio...";
        btn.disabled = true;
        btn.style.opacity = "0.7";

        try {
            await invoke('start_server', { id: serverId });
            
            // Successo Avvio
            btn.textContent = "Stop Server";
            btn.style.backgroundColor = "var(--danger)"; // Rosso
            btn.disabled = false;
            btn.style.opacity = "1";
            
            runningServers.add(serverId);
            
            // Aggiorna status visivo
            document.getElementById("detail-status").textContent = "STARTING...";
            document.getElementById("detail-status").style.color = "var(--warning)";
            document.getElementById(`status-dot-${serverId.toLowerCase().replace(/\s+/g, '-')}`).style.backgroundColor = "var(--warning)";
            serverStatus = "online";
        } catch (error) {
            alert("Errore Start: " + error);
            btn.textContent = "Avvia Server";
            btn.disabled = false;
            btn.style.opacity = "1";
        }

    } else {
        // --- STOP LOGIC ---
        btn.textContent = "Spegnimento...";
        btn.disabled = true;

        try {
            await invoke('stop_server', { id: serverId });
            
            // Simuliamo un'attesa per la chiusura
            setTimeout(() => {
                btn.textContent = "Avvia Server";
                btn.style.backgroundColor = "var(--success)"; // O il colore primario verde
                btn.disabled = false;
                
                runningServers.delete(serverId);
                
                document.getElementById("detail-status").textContent = "OFFLINE";
                document.getElementById("detail-status").style.color = "var(--danger)";
                document.getElementById(`status-dot-${serverId.toLowerCase().replace(/\s+/g, '-')}`).style.backgroundColor = "var(--danger)";
                serverStatus = "offline";
            }, 3000); // Aspetta 3 secondi finti mentre il server salva e chiude

        } catch (error) {
            alert("Errore Stop: " + error);
            btn.textContent = "Stop Server"; // Torna allo stato di prima
            btn.disabled = false;
        }
    }
}

function selectServer(server) {
  state.activeServerId = server.id;
  emptyStateEl.classList.add("hidden");
  serverDetailsEl.classList.remove("hidden");

  updateBannerUI(server);
  populatePropertiesPanel(server.properties);
  renderModsList(server.mods);

  // --- NEW: Console Setup ---
  clearConsole();
  
  // Setup Button (Unchanged)
  const btnStart = document.querySelector('.btn-start');
  const newBtn = btnStart.cloneNode(true);
  const statusDot = document.getElementById(`status-dot-${server.id.toLowerCase().replace(/\s+/g, '-')}`);
  btnStart.parentNode.replaceChild(newBtn, btnStart);

  if(server.status === 'online') 
    runningServers.add(server.id);

  if (runningServers.has(server.id)) {
    newBtn.textContent = "Stop Server";
    newBtn.style.backgroundColor = "var(--danger)";
    statusDot.style.backgroundColor = "var(--success)";
  } else {
    newBtn.textContent = "Avvia Server";
    newBtn.style.backgroundColor = ""; 
    statusDot.style.backgroundColor = "var(--danger)";
  }

  newBtn.addEventListener('click', () => {
    handleServerToggle(server.id, server.status, newBtn);
  });
}

function updateBannerUI(server) {
  detailNameEl.textContent = server.name;
  detailVersionEl.textContent = server.version;
  detailIconImg.src = `./assets/${server.icon}`;
  
  if(server.status === 'online') {
    detailStatusEl.textContent = "ONLINE";
    detailStatusEl.style.color = "var(--success)";
  } else {
    detailStatusEl.textContent = "OFFLINE";
    detailStatusEl.style.color = "var(--danger)";
  }
}

async function initApp() {
  setupTabs();
  setupModals(state, renderSidebar, updateBannerUI);
  
  // Initialize Console Listeners
  await setupConsole(state); 

  serverListEl.classList.add("hidden"); 
  loadingEl.classList.remove("hidden");

  try {
    state.serverList = await getServers();
    renderSidebar();
  } catch (e) {
    serverListEl.innerHTML = `<div style='color:var(--danger); text-align:center;'>Errore: ${e}</div>`;
  } finally {
    loadingEl.classList.add("hidden");
    serverListEl.classList.remove("hidden");
  }
}

window.addEventListener("DOMContentLoaded", initApp);