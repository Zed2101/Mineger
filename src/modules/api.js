const { invoke } = window.__TAURI__.core;

export async function getServers() {
  try {
    return await invoke('get_servers');
  } catch (error) {
    console.error("Backend Error:", error);
    throw error;
  }
}

// In the future: export async function startServer(id) { ... }